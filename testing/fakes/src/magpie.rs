//! fake-magpie: `magpie_api::server::Server` with scripted answers on every route, and a model of the sandbox
//! surface (magpie 6, 8.4, 11.2, 16.2) for the supervisor's tests.
//!
//! - **Every route** answers what the test scripted ([`crate::script`]): straight from the golden fixtures
//!   ([`FakeMagpie::script_fixture`]) or by hand. pigeon's (the person routes), hermit's and the ops routes have no
//!   model: unscripted, they answer 501 `not_supported`. Every `/v1/sandbox/**` route has one.
//! - **The sandbox surface** has a model, per conversation (from `Aviary-Sandbox-Conversation`): the lease (a fresh
//!   id and epoch per grant, every earlier lease dead at once, `not_current_sandbox` when the test names another
//!   sandbox as hermit's current one), `~/.claude` as files and directories (the write, truncate, rename, unlink,
//!   mkdir, rmdir and setattr rules of magpie 6.6-6.8 as the supervisor sees them: rule 0 (a write whose range is
//!   inside the file and already holds exactly those bytes answers the current `{size, version}` and changes
//!   nothing, checked first), `offset_beyond_eof`, a missing parent 404, `protected` main transcript, `exists`,
//!   `not_empty`, the quota, `opId` replays answering the stored result), the command queue with its long-poll and
//!   acks, blobs by sha256, `SendUserFile` snapshots (`sent-files` stages a send per `toolUseId` and answers the shas
//!   not stored yet; the commit needs every blob, answers `{kept, seq}` once and the same answer on a repeat;
//!   [`FakeMagpie::sent`]), and the reports (accepted and recorded; `turn_state` `running` sets the main
//!   transcript). It keeps no events, rows or timeline.
//!
//! - **Callers** ([`FakeMagpie::new`]): a generated pair token per caller of magpie's route table; an unknown bearer
//!   answers 401 `unauthenticated` and a caller the route does not list 403 `wrong_caller`, before any scripted
//!   answer. [`FakeMagpie::unguarded`] checks nothing. Use [`Running::client`] to speak as a caller.
//! - **Renames** follow `rename(2)`: onto an existing empty directory (from a directory) or a file (from a file)
//!   it replaces the target; onto a non-empty directory 409 `not_empty`; a file onto a directory or the other way
//!   409 `exists`; a directory into its own subtree 400 `bad_path`.
//! - **opId**: a committed result is stored and a replay answers it; a refused operation is not stored and changes
//!   nothing, a refused create included (magpie rolls the transaction back, magpie 6.5), so a retry after a
//!   refusal runs again.
//!
//! Deliberate differences from magpie, and codes the model never produces (script them with
//! [`crate::script`] or [`crate::fault`] when a test needs them):
//!
//! - with no current sandbox named ([`FakeMagpie::set_current_sandbox`]) the lease is granted to any sandbox
//!   ([`NoCurrentSandbox::Grant`], the default); magpie refuses `not_current_sandbox` when hermit names none, which
//!   [`FakeMagpie::with_lease_mode`]`(`[`NoCurrentSandbox::Refuse`]`)` models;
//! - 409 `conversation_migrating` (no migrations), 503 `unavailable` on the lease (hermit always answers; use the
//!   `Unavailable` fault), 413 `too_large` for the per-line cap (only the 16 MiB body cap is modelled), 409
//!   `offset_mismatch` (an append at a stale offset inside the file overwrites, as `pwrite` would; only
//!   `offset_beyond_eof` is refused);
//! - `GET /v1/sandbox/blobs/{sha}` is unscoped: it answers any stored blob to any conversation with or without a
//!   lease, and `as` is ignored (the stored bytes); magpie answers only blobs the sandbox's conversation
//!   references, and decodes for `as=media|text`;
//! - M9 (`timeline`, `admin_timeline`) is scripted, but the query rule magpie checks first is modelled: `liveAfter`
//!   without `liveTurn` (or the other way round), or `waitMs`/`liveAfter`/`liveTurn` without `after`, answers 400
//!   `bad_request` before any scripted answer, so a caller that forgets `liveTurn` fails its test;
//! - a sent-files commit missing a blob answers 400 `bad_request` (magpie 11.2 names no code), and the commit's
//!   `seq` counts the conversation's commits from 1, since the fake writes no `files.sent` event.
//!
//! Faults ([`crate::fault`]) apply by route id or path template.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use aviary_core::auth::{Caller, Callers};
use aviary_core::http::ApiError;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use magpie_api::changes::{ChangesQuery, ChangesResponse};
use magpie_api::client::Client;
use magpie_api::conversations::{Conversation, CreateRequest, CreateResponse, ListQuery, ListResponse, RenameRequest};
use magpie_api::errors::{ErrorCode, Refusal};
use magpie_api::events::{EventsPage, EventsQuery};
use magpie_api::files::{FilesPanel, RawFileQuery};
use magpie_api::legacy::{
    LegacyPathRequest, TicketClaim, TicketClaimed, TicketDone, TicketRequest, TicketResponse, WindowState,
};
use magpie_api::live::{LiveFramesRequest, LiveState};
use magpie_api::ops::{
    AdminConversation, AdminConversationList, AdminListQuery, LegacyImportStatus, OpsStats, PeopleQuery,
    PeopleResponse, PersonPurgeRequest, PersonPurgeResponse, ViewAsLog, ViewAsLogQuery, ViewAsRequest,
    WorkspacesExistsRequest, WorkspacesExistsResponse,
};
use magpie_api::rows::{PromptsResponse, SearchQuery, SearchResponse, TimelinePage, TimelineQuery};
use magpie_api::sandbox::{
    AckRequest, AckResult, AgentTurnRequest, AskReport, AskResolvedReport, CC_FILE_MAX_BYTES, CC_QUOTA_BYTES,
    CC_WRITE_MAX_BYTES, CcEntry, CcFileQuery, CcKind, CcMkdir, CcPathOp, CcRename, CcSetattr, CcSizeVersion, CcTree,
    CcTruncate, CcVersion, CcWriteQuery, Command, CommandBody, CommandsQuery, CommandsResponse, Heartbeat,
    LeaseResponse, ReportedState, SentFilesCommitted, SentFilesRequest, SentFilesResponse, TasksReport,
    TurnStateReport,
};
use magpie_api::server::{Ctx, Server};
use magpie_api::shares::{ContinueResponse, ShareRef, ShareView, SharesList};
use magpie_api::turns::{AgentPermissionResponse, CancelResponse, SendRequest, SendResponse};
use magpie_api::uploads::{BlobQuery, SandboxBlobQuery, UploadMeta, UploadResponse};
use magpie_api::{Admitted, Empty};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::Digest;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::answer::{admitted, not_supported, option};
use crate::fault::{self, Faults, Names};
use crate::fixtures;
use crate::script::{Call, Reply, Script};
use crate::serve::{Served, serve};

/// The longest a command long-poll is held (magpie 8.4: `waitMs` up to 25 s).
pub const MAX_COMMANDS_WAIT: Duration = Duration::from_secs(25);

fn refuse(code: ErrorCode, message: &str) -> ApiError {
    Refusal::new(code, message).into()
}

fn lease_lost() -> ApiError {
    refuse(ErrorCode::LeaseLost, "the lease is not current")
}

fn not_found(message: &str) -> ApiError {
    refuse(ErrorCode::NotFound, message)
}

/// A committed `SendUserFile` snapshot (magpie 11.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentSend {
    /// The request the commit kept (paths with a `file://` prefix stripped).
    pub request: SentFilesRequest,
    /// The commit's answer.
    pub committed: SentFilesCommitted,
}

/// A file or directory of one conversation's `~/.claude`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CcNode {
    /// `lines`, `bytes` or `dir`.
    pub kind: CcKind,
    /// The file's bytes (empty for a directory).
    pub data: Vec<u8>,
    /// `mtimeNs` as last set.
    pub mtime_ns: i64,
    /// The mode bits.
    pub mode: u32,
    /// +1 on every change.
    pub version: u64,
}

/// magpie 6.2: a lines file is any path whose last component ends in `.jsonl` or contains `.jsonl.`.
#[must_use]
pub fn kind_of(path: &str) -> CcKind {
    let last = path.rsplit('/').next().unwrap_or(path);
    #[allow(clippy::case_sensitive_file_extension_comparisons)] // magpie's rule is by exact name
    if last.ends_with(".jsonl") || last.contains(".jsonl.") {
        CcKind::Lines
    } else {
        CcKind::Bytes
    }
}

fn parent(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(p, _)| p)
}

/// At most this many files in one send (magpie 11.2).
const SENT_FILES_MAX: usize = 100;
/// Each sent file's cap (magpie 11.2: the app's download cap).
const SENT_FILE_MAX_BYTES: u64 = 64 << 20;
/// One send's total cap.
const SENT_TOTAL_MAX_BYTES: u64 = 256 << 20;

fn check_tool_use_id(id: &str) -> Result<(), ApiError> {
    let ok = (1..=128).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(ApiError::bad_request("toolUseId is not ^[A-Za-z0-9_-]{1,128}$"))
    }
}

/// Absolute under `/workspace`, normalised, no `..`, no NUL, at most 4,096 bytes (magpie 11.2).
fn valid_workspace_path(path: &str) -> bool {
    path.len() <= 4096 && !path.contains('\0') && path.strip_prefix("/workspace/").is_some_and(valid_path)
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.ends_with('/')
        && path.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

/// One conversation's sandbox-side state in the model.
#[derive(Debug, Default)]
struct Conv {
    lease: Option<Uuid>,
    epoch: u64,
    current_sandbox: Option<String>,
    provider_session_id: Option<String>,
    main_transcript: Option<String>,
    resync: bool,
    nodes: BTreeMap<String, CcNode>,
    ops: HashMap<Uuid, Value>,
    commands: Vec<Command>,
    acks: BTreeMap<u64, AckResult>,
    agent_turns: HashSet<Uuid>,
    /// `sent-files` requests waiting for their commit, by `toolUseId`.
    sent_staging: HashMap<String, SentFilesRequest>,
    /// Committed sends, by `toolUseId`.
    sent: BTreeMap<String, SentSend>,
    /// The last `seq` a commit answered (the fake keeps no events; commits count from 1).
    sent_seq: u64,
}

impl Conv {
    fn cc_bytes(&self) -> u64 {
        self.nodes.values().map(|n| n.data.len() as u64).sum()
    }

    fn dir_exists(&self, path: Option<&str>) -> bool {
        match path {
            None => true,
            Some(p) => self.nodes.get(p).is_some_and(|n| n.kind == CcKind::Dir),
        }
    }

    fn children(&self, dir: &str) -> impl Iterator<Item = &String> {
        let prefix = format!("{dir}/");
        self.nodes.keys().filter(move |k| k.starts_with(&prefix))
    }
}

#[derive(Debug)]
struct Model {
    convs: HashMap<Uuid, Conv>,
    blobs: HashMap<String, Bytes>,
    quota: u64,
}

/// The fake magpie.
#[derive(Debug)]
pub struct FakeMagpie {
    script: Script,
    faults: Arc<Faults>,
    model: Mutex<Model>,
    commands_changed: Notify,
    tokens: Option<HashMap<Caller, String>>,
    lease_open: bool,
}

/// How the fake grants a lease when the test named no current sandbox ([`FakeMagpie::set_current_sandbox`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NoCurrentSandbox {
    /// Grant any sandbox (the default: a supervisor test need not set hermit's view first). Real magpie asks hermit
    /// and refuses 409 `not_current_sandbox` when hermit names none.
    #[default]
    Grant,
    /// Refuse 409 `not_current_sandbox`, as magpie does when hermit names no current sandbox.
    Refuse,
}

/// A [`FakeMagpie`] served on a local port.
#[derive(Debug)]
pub struct Running {
    /// The fake (script it, inspect it).
    pub fake: Arc<FakeMagpie>,
    /// The listener.
    pub served: Served,
}

impl Running {
    /// The base URL.
    #[must_use]
    pub fn url(&self) -> String {
        self.served.url()
    }

    /// A magpie-api client speaking as `caller` (its pair token; any token when unguarded).
    #[must_use]
    pub fn client(&self, http: reqwest::Client, caller: Caller) -> Client {
        let token = self.fake.token(caller).unwrap_or_else(|| "unguarded".to_owned());
        Client::new(http, self.url(), token)
    }
}

fn names() -> Names {
    Arc::new(|method: &Method, template: &str| {
        magpie_api::routes::ROUTES
            .iter()
            .find(|r| r.path == template && r.method.as_str() == method.as_str())
            .map(|r| r.id)
    })
}

/// magpie's caller check: the pair token names the caller (401 `unauthenticated` otherwise), and the matched route
/// must list it (403 `wrong_caller` otherwise).
async fn check_caller(State(callers): State<Callers>, mut req: Request, next: Next) -> Response {
    let template = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned())
        .unwrap_or_default();
    let Some(route) = magpie_api::routes::ROUTES
        .iter()
        .find(|r| r.path == template && r.method.as_str() == req.method().as_str())
    else {
        // a method the route table does not have: magpie-api's router answers it (405)
        return next.run(req).await;
    };
    let Some(caller) = callers.identify_headers(req.headers()) else {
        return ApiError::unauthenticated().into_response();
    };
    if !route.callers.contains(&caller) {
        return ApiError::wrong_caller(caller).into_response();
    }
    req.extensions_mut().insert(caller);
    next.run(req).await
}

fn ser<T: Serialize>(v: &T) -> Option<Value> {
    serde_json::to_value(v).ok()
}

impl FakeMagpie {
    /// A fake with nothing scripted, an empty model and a generated pair token per caller of magpie's routes
    /// ([`FakeMagpie::token`]): its router refuses an unknown bearer with 401 `unauthenticated` and a caller the
    /// route table ([`magpie_api::routes::Route::callers`]) does not list with 403 `wrong_caller`, as magpie does.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Self::with(NoCurrentSandbox::Grant, true)
    }

    /// A guarded fake whose lease follows `mode` when no current sandbox is named.
    #[must_use]
    pub fn with_lease_mode(mode: NoCurrentSandbox) -> Arc<Self> {
        Self::with(mode, true)
    }

    /// A fake whose router checks no caller tokens (magpie-api's plain `router`).
    #[must_use]
    pub fn unguarded() -> Arc<Self> {
        Self::with(NoCurrentSandbox::Grant, false)
    }

    fn with(mode: NoCurrentSandbox, guarded: bool) -> Arc<Self> {
        let tokens = guarded.then(|| {
            Caller::ALL
                .iter()
                .filter(|c| magpie_api::routes::ROUTES.iter().any(|r| r.callers.contains(c)))
                .map(|c| (*c, format!("fake-magpie-{}-{}", c.as_str(), Uuid::new_v4().simple())))
                .collect()
        });
        Arc::new(Self {
            script: Script::default(),
            faults: Arc::new(Faults::default()),
            model: Mutex::new(Model {
                convs: HashMap::new(),
                blobs: HashMap::new(),
                quota: CC_QUOTA_BYTES,
            }),
            commands_changed: Notify::new(),
            tokens,
            lease_open: mode == NoCurrentSandbox::Grant,
        })
    }

    /// The pair token `caller` presents to this fake (`None` when unguarded or when no route admits `caller`).
    #[must_use]
    pub fn token(&self, caller: Caller) -> Option<String> {
        self.tokens.as_ref().and_then(|t| t.get(&caller).cloned())
    }

    /// The router: magpie-api's, behind the caller check when guarded, with the faults applied.
    ///
    /// # Panics
    /// Never: the generated tokens are distinct and non-empty.
    pub fn router(self: &Arc<Self>) -> Router {
        let router = magpie_api::server::router(self.clone());
        let router = match &self.tokens {
            Some(t) => {
                let callers = Callers::new(t.iter().map(|(c, tok)| (*c, vec![tok.clone()])))
                    .unwrap_or_else(|e| panic!("fake-magpie tokens: {e}"));
                router.route_layer(axum::middleware::from_fn_with_state(callers, check_caller))
            }
            None => router,
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

    /// Queue response `index` of `fixtures/routes/<route>.json` as the next answer of `route`.
    ///
    /// # Panics
    /// When the fixture or the index is missing.
    pub fn script_fixture(&self, route: &str, index: usize) {
        let replies = fixtures::magpie_replies(route);
        let r = replies
            .get(index)
            .unwrap_or_else(|| panic!("{route}: no response {index}"))
            .clone();
        self.script.once(route, r);
    }

    fn lock(&self) -> MutexGuard<'_, Model> {
        self.model.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn record(
        &self,
        route: &'static str,
        ctx: &Ctx,
        params: Value,
        query: Option<Value>,
        body: Option<Value>,
        bytes: Option<Bytes>,
    ) -> Option<Reply> {
        self.script.take(Call {
            route,
            params,
            query,
            body,
            bytes,
            headers: ctx.headers.clone(),
        })
    }

    // ---- the model's test controls ----

    /// The quota of every conversation's `~/.claude` (magpie 17: 2 GiB).
    pub fn set_quota(&self, bytes: u64) {
        self.lock().quota = bytes;
    }

    /// Name hermit's current sandbox of `conversation`: a lease asked by any other sandbox answers 409
    /// `not_current_sandbox`. `None` (the default) follows the fake's [`NoCurrentSandbox`] mode: by default every
    /// sandbox is granted, which real magpie does not do.
    pub fn set_current_sandbox(&self, conversation: Uuid, sandbox: Option<&str>) {
        self.lock().convs.entry(conversation).or_default().current_sandbox = sandbox.map(str::to_owned);
    }

    /// Make the next lease of `conversation` answer `resync: true` (magpie 6.12).
    pub fn set_resync(&self, conversation: Uuid) {
        self.lock().convs.entry(conversation).or_default().resync = true;
    }

    /// Set the provider session (and so the main transcript path) as the importer or a `running` report would.
    pub fn set_provider_session(&self, conversation: Uuid, session: &str) {
        let mut m = self.lock();
        let c = m.convs.entry(conversation).or_default();
        c.provider_session_id = Some(session.to_owned());
        c.main_transcript = Some(format!("projects/-workspace/{session}.jsonl"));
    }

    /// Put a file (and its parent directories) into `conversation`'s `~/.claude` without an operation.
    pub fn seed_file(&self, conversation: Uuid, path: &str, data: &[u8]) {
        let mut m = self.lock();
        let c = m.convs.entry(conversation).or_default();
        let mut dir = String::new();
        if let Some(p) = parent(path) {
            for part in p.split('/') {
                if !dir.is_empty() {
                    dir.push('/');
                }
                dir.push_str(part);
                c.nodes.entry(dir.clone()).or_insert(CcNode {
                    kind: CcKind::Dir,
                    data: Vec::new(),
                    mtime_ns: 0,
                    mode: 0o700,
                    version: 1,
                });
            }
        }
        let version = c.nodes.get(path).map_or(1, |n| n.version + 1);
        c.nodes.insert(
            path.to_owned(),
            CcNode {
                kind: kind_of(path),
                data: data.to_vec(),
                mtime_ns: 0,
                mode: 0o600,
                version,
            },
        );
    }

    /// Sets an existing file's bytes and `version` without an operation: magpie as a restore left it, at an older
    /// version that lacks writes it had acknowledged (the supervisor's repair, supervisor 9.5 item 6). A file that
    /// does not exist is seeded first.
    pub fn restore_file(&self, conversation: Uuid, path: &str, data: &[u8], version: u64) {
        if self.node(conversation, path).is_none() {
            self.seed_file(conversation, path, data);
        }
        let mut m = self.lock();
        if let Some(n) = m.convs.get_mut(&conversation).and_then(|c| c.nodes.get_mut(path)) {
            n.data = data.to_vec();
            n.version = version;
        }
    }

    /// A node of `conversation`'s `~/.claude`.
    #[must_use]
    pub fn node(&self, conversation: Uuid, path: &str) -> Option<CcNode> {
        self.lock().convs.get(&conversation)?.nodes.get(path).cloned()
    }

    /// Every path of `conversation`'s `~/.claude`, sorted.
    #[must_use]
    pub fn paths(&self, conversation: Uuid) -> Vec<String> {
        self.lock()
            .convs
            .get(&conversation)
            .map(|c| c.nodes.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// The current lease epoch of `conversation` (0 before the first grant).
    #[must_use]
    pub fn lease_epoch(&self, conversation: Uuid) -> u64 {
        self.lock().convs.get(&conversation).map_or(0, |c| c.epoch)
    }

    /// Queue a command for `conversation`'s supervisor; its `cmdSeq`. Wakes held command polls.
    pub fn push_command(&self, conversation: Uuid, turn_id: Option<Uuid>, body: CommandBody) -> u64 {
        let seq = {
            let mut m = self.lock();
            let c = m.convs.entry(conversation).or_default();
            let seq = c.commands.last().map_or(1, |l| l.cmd_seq + 1);
            c.commands.push(Command {
                cmd_seq: seq,
                turn_id,
                body,
            });
            seq
        };
        self.commands_changed.notify_waiters();
        seq
    }

    /// The acks received for `conversation`'s commands, by `cmdSeq`.
    #[must_use]
    pub fn acks(&self, conversation: Uuid) -> BTreeMap<u64, AckResult> {
        self.lock()
            .convs
            .get(&conversation)
            .map(|c| c.acks.clone())
            .unwrap_or_default()
    }

    /// The committed sends of `conversation`, by `toolUseId`.
    #[must_use]
    pub fn sent(&self, conversation: Uuid) -> BTreeMap<String, SentSend> {
        self.lock()
            .convs
            .get(&conversation)
            .map(|c| c.sent.clone())
            .unwrap_or_default()
    }

    /// A stored blob.
    #[must_use]
    pub fn blob(&self, sha: &str) -> Option<Bytes> {
        self.lock().blobs.get(sha).cloned()
    }

    /// Store a blob (as the person's upload would).
    pub fn seed_blob(&self, data: &[u8]) -> String {
        let sha = hex_digest(&sha2::Sha256::digest(data));
        self.lock().blobs.insert(sha.clone(), Bytes::from(data.to_vec()));
        sha
    }

    // ---- the model ----

    fn conversation(ctx: &Ctx) -> Result<Uuid, ApiError> {
        ctx.sandbox
            .as_ref()
            .and_then(|s| Uuid::parse_str(&s.conversation).ok())
            .ok_or_else(|| ApiError::bad_request("fake: no Aviary-Sandbox-* identity"))
    }

    fn with_conv<T>(
        &self,
        ctx: &Ctx,
        lease: bool,
        f: impl FnOnce(&mut Conv, u64) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        let id = Self::conversation(ctx)?;
        let mut m = self.lock();
        let quota = m.quota;
        let c = m.convs.entry(id).or_default();
        if lease {
            let presented = ctx.lease.as_deref().and_then(|l| Uuid::parse_str(l).ok());
            if presented.is_none() || presented != c.lease {
                return Err(lease_lost());
            }
        }
        f(c, quota)
    }

    /// Run an operation once per `opId`: a replay answers the stored result.
    fn op<T: Serialize + serde::de::DeserializeOwned>(
        &self,
        ctx: &Ctx,
        op_id: Uuid,
        f: impl FnOnce(&mut Conv, u64) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        self.with_conv(ctx, true, |c, quota| {
            if let Some(v) = c.ops.get(&op_id) {
                return serde_json::from_value(v.clone()).map_err(|e| ApiError::internal(e.to_string()));
            }
            let r = f(c, quota)?;
            c.ops.insert(op_id, serde_json::to_value(&r).unwrap_or(Value::Null));
            Ok(r)
        })
    }

    fn model_lease(&self, ctx: &Ctx) -> Result<LeaseResponse, ApiError> {
        let sandbox = ctx.sandbox.as_ref().map(|s| s.id.clone());
        let open = self.lease_open;
        self.with_conv(ctx, false, |c, _| {
            let refused = match &c.current_sandbox {
                Some(current) => sandbox.as_ref() != Some(current),
                None => !open,
            };
            if refused {
                return Err(refuse(
                    ErrorCode::NotCurrentSandbox,
                    "hermit names another sandbox for this conversation",
                ));
            }
            let lease = Uuid::new_v4();
            c.lease = Some(lease);
            c.epoch += 1;
            let resync = std::mem::take(&mut c.resync);
            Ok(LeaseResponse {
                lease_id: lease,
                epoch: c.epoch,
                conversation_id: Uuid::nil(),
                provider_session_id: c.provider_session_id.clone(),
                main_transcript: c.main_transcript.clone(),
                resync,
            })
        })
        .map(|mut l| {
            l.conversation_id = Self::conversation(ctx).unwrap_or_default();
            l
        })
    }

    fn model_tree(&self, ctx: &Ctx) -> Result<CcTree, ApiError> {
        self.with_conv(ctx, false, |c, quota| {
            Ok(CcTree {
                files: c
                    .nodes
                    .iter()
                    .map(|(p, n)| CcEntry {
                        path: p.clone(),
                        kind: n.kind,
                        size: n.data.len() as u64,
                        mtime_ns: n.mtime_ns,
                        mode: n.mode,
                        version: n.version,
                    })
                    .collect(),
                cc_bytes: c.cc_bytes(),
                quota_bytes: quota,
            })
        })
    }

    fn model_file(&self, ctx: &Ctx, q: &CcFileQuery) -> Result<Response, ApiError> {
        let (data, ranged) = self.with_conv(ctx, false, |c, _| {
            let n = c
                .nodes
                .get(&q.path)
                .filter(|n| n.kind != CcKind::Dir)
                .ok_or_else(|| not_found("not found"))?;
            let len = n.data.len() as u64;
            let start = q.offset.unwrap_or(0).min(len);
            let end = q.length.map_or(len, |l| start.saturating_add(l).min(len));
            let ranged = q.offset.is_some() || q.length.is_some();
            #[allow(clippy::cast_possible_truncation)] // in-memory sizes
            Ok((n.data[start as usize..end as usize].to_vec(), ranged))
        })?;
        let status = if ranged {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        };
        let mut res = (status, data).into_response();
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        Ok(res)
    }

    fn model_write(&self, ctx: &Ctx, q: &CcWriteQuery, data: &[u8]) -> Result<CcSizeVersion, ApiError> {
        if !valid_path(&q.path) {
            return Err(refuse(ErrorCode::BadPath, "bad path"));
        }
        if data.len() > CC_WRITE_MAX_BYTES {
            return Err(refuse(ErrorCode::TooLarge, "the write is over 16 MiB"));
        }
        self.op(ctx, q.op_id, |c, quota| {
            // magpie 6.6 rule 0, before every other rule: a range inside the file that already holds exactly these
            // bytes is a success that changes nothing (the sentry's resend under a new opId applies once)
            if let Some(n) = c.nodes.get(&q.path).filter(|n| n.kind != CcKind::Dir) {
                let size = n.data.len() as u64;
                let end = q.offset.saturating_add(data.len() as u64);
                #[allow(clippy::cast_possible_truncation)] // in-memory sizes
                if end <= size && n.data[q.offset as usize..end as usize] == *data {
                    return Ok(CcSizeVersion {
                        size,
                        version: n.version,
                    });
                }
            }
            if !c.dir_exists(parent(&q.path)) {
                return Err(not_found("the parent directory does not exist"));
            }
            // every check runs before a new file is inserted, so a refused create leaves no path behind
            let size = match c.nodes.get(&q.path) {
                Some(n) if n.kind == CcKind::Dir => return Err(ApiError::bad_request("a directory")),
                Some(n) => n.data.len() as u64,
                None if q.create => 0,
                None => return Err(not_found("not found")),
            };
            if q.offset > size {
                return Err(refuse(ErrorCode::OffsetBeyondEof, "the offset is past the end"));
            }
            // an append expects the file's size (magpie 6.10)
            if q.append && q.offset != size {
                return Err(refuse(
                    ErrorCode::OffsetMismatch,
                    "the append's offset is not the file's size",
                ));
            }
            let end = q.offset + data.len() as u64;
            let new_size = end.max(size);
            // the 256 MiB cap is a byte file's; a lines file is bounded by the quota (magpie 17)
            if kind_of(&q.path) == CcKind::Bytes && new_size > CC_FILE_MAX_BYTES {
                return Err(refuse(ErrorCode::TooLarge, "the file is over 256 MiB"));
            }
            if c.cc_bytes() - size + new_size > quota {
                return Err(refuse(
                    ErrorCode::QuotaExceeded,
                    "the conversation's Claude Code state is over its quota",
                ));
            }
            let node = c.nodes.entry(q.path.clone()).or_insert(CcNode {
                kind: kind_of(&q.path),
                data: Vec::new(),
                mtime_ns: q.mtime_ns,
                mode: q.mode,
                version: 0,
            });
            #[allow(clippy::cast_possible_truncation)] // in-memory sizes
            {
                node.data.resize(new_size as usize, 0);
                node.data[q.offset as usize..end as usize].copy_from_slice(data);
            }
            node.mtime_ns = q.mtime_ns;
            node.version += 1;
            Ok(CcSizeVersion {
                size: new_size,
                version: node.version,
            })
        })
    }

    fn model_truncate(&self, ctx: &Ctx, b: &CcTruncate) -> Result<CcSizeVersion, ApiError> {
        self.op(ctx, b.op_id, |c, quota| {
            let used = c.cc_bytes();
            let n = c
                .nodes
                .get_mut(&b.path)
                .filter(|n| n.kind != CcKind::Dir)
                .ok_or_else(|| not_found("not found"))?;
            let size = n.data.len() as u64;
            if b.size > size && n.kind == CcKind::Lines {
                return Err(refuse(ErrorCode::OffsetBeyondEof, "the size is past the end"));
            }
            if b.size > size && used - size + b.size > quota {
                return Err(refuse(ErrorCode::QuotaExceeded, "over the quota"));
            }
            #[allow(clippy::cast_possible_truncation)] // in-memory sizes
            n.data.resize(b.size as usize, 0);
            n.mtime_ns = b.mtime_ns;
            n.version += 1;
            Ok(CcSizeVersion {
                size: b.size,
                version: n.version,
            })
        })
    }

    fn model_rename(&self, ctx: &Ctx, b: &CcRename) -> Result<CcVersion, ApiError> {
        if !valid_path(&b.to) {
            return Err(refuse(ErrorCode::BadPath, "bad path"));
        }
        self.op(ctx, b.op_id, |c, _| {
            let from = c.nodes.get(&b.from).cloned().ok_or_else(|| not_found("not found"))?;
            if !c.dir_exists(parent(&b.to)) {
                return Err(not_found("the parent directory does not exist"));
            }
            if b.to == b.from {
                return Ok(CcVersion { version: from.version });
            }
            let from_dir = from.kind == CcKind::Dir;
            if from_dir && b.to.starts_with(&format!("{}/", b.from)) {
                return Err(refuse(
                    ErrorCode::BadPath,
                    "a directory cannot move into its own subtree",
                ));
            }
            if let Some(target) = c.nodes.get(&b.to) {
                let target_dir = target.kind == CcKind::Dir;
                if target_dir && c.children(&b.to).next().is_some() {
                    return Err(refuse(ErrorCode::NotEmpty, "the target directory is not empty"));
                }
                if target_dir != from_dir {
                    return Err(refuse(
                        ErrorCode::Exists,
                        "a file and a directory cannot replace each other",
                    ));
                }
            }
            if from_dir {
                let moved: Vec<String> = c.children(&b.from).cloned().collect();
                for old in moved {
                    if let Some(n) = c.nodes.remove(&old) {
                        let new = format!("{}{}", b.to, &old[b.from.len()..]);
                        c.nodes.insert(new, n);
                    }
                }
            }
            c.nodes.remove(&b.from);
            let version = c.nodes.get(&b.to).map_or(0, |n| n.version).max(from.version) + 1;
            c.nodes.insert(
                b.to.clone(),
                CcNode {
                    kind: if from.kind == CcKind::Dir {
                        CcKind::Dir
                    } else {
                        kind_of(&b.to)
                    },
                    version,
                    ..from
                },
            );
            Ok(CcVersion { version })
        })
    }

    fn model_unlink(&self, ctx: &Ctx, b: &CcPathOp) -> Result<(), ApiError> {
        self.op(ctx, b.op_id, |c, _| {
            if c.main_transcript.as_deref() == Some(b.path.as_str()) {
                return Err(refuse(ErrorCode::Protected, "the main transcript cannot be removed"));
            }
            match c.nodes.get(&b.path) {
                Some(n) if n.kind != CcKind::Dir => {
                    c.nodes.remove(&b.path);
                    Ok(())
                }
                _ => Err(not_found("not found")),
            }
        })
    }

    fn model_mkdir(&self, ctx: &Ctx, b: &CcMkdir) -> Result<(), ApiError> {
        if !valid_path(&b.path) {
            return Err(refuse(ErrorCode::BadPath, "bad path"));
        }
        self.op(ctx, b.op_id, |c, _| {
            if c.nodes.contains_key(&b.path) {
                return Err(refuse(ErrorCode::Exists, "the path exists"));
            }
            if !c.dir_exists(parent(&b.path)) {
                return Err(not_found("the parent directory does not exist"));
            }
            c.nodes.insert(
                b.path.clone(),
                CcNode {
                    kind: CcKind::Dir,
                    data: Vec::new(),
                    mtime_ns: b.mtime_ns,
                    mode: b.mode,
                    version: 1,
                },
            );
            Ok(())
        })
    }

    fn model_rmdir(&self, ctx: &Ctx, b: &CcPathOp) -> Result<(), ApiError> {
        self.op(ctx, b.op_id, |c, _| {
            match c.nodes.get(&b.path) {
                Some(n) if n.kind == CcKind::Dir => {}
                _ => return Err(not_found("not found")),
            }
            if c.children(&b.path).next().is_some() {
                return Err(refuse(ErrorCode::NotEmpty, "the directory is not empty"));
            }
            c.nodes.remove(&b.path);
            Ok(())
        })
    }

    fn model_setattr(&self, ctx: &Ctx, b: &CcSetattr) -> Result<CcVersion, ApiError> {
        self.op(ctx, b.op_id, |c, _| {
            let n = c.nodes.get_mut(&b.path).ok_or_else(|| not_found("not found"))?;
            if let Some(m) = b.mode {
                n.mode = m;
            }
            if let Some(t) = b.mtime_ns {
                n.mtime_ns = t;
            }
            n.version += 1;
            Ok(CcVersion { version: n.version })
        })
    }

    async fn model_commands(&self, ctx: &Ctx, q: CommandsQuery) -> Result<CommandsResponse, ApiError> {
        let wait = Duration::from_millis(q.wait_ms.unwrap_or(0)).min(MAX_COMMANDS_WAIT);
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let changed = self.commands_changed.notified();
            let found = self.with_conv(ctx, true, |c, _| {
                let head = c.commands.last().map_or(0, |l| l.cmd_seq);
                // as magpie 8.4: neither canceled nor acknowledged
                let cmds: Vec<Command> = c
                    .commands
                    .iter()
                    .filter(|x| x.cmd_seq > q.after && !c.acks.contains_key(&x.cmd_seq))
                    .cloned()
                    .collect();
                Ok(CommandsResponse { commands: cmds, head })
            })?;
            if !found.commands.is_empty() || tokio::time::Instant::now() >= deadline {
                return Ok(found);
            }
            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                // one last look, then the timeout answer (the lease is checked again)
                return self.with_conv(ctx, true, |c, _| {
                    let head = c.commands.last().map_or(0, |l| l.cmd_seq);
                    Ok(CommandsResponse {
                        commands: c
                            .commands
                            .iter()
                            .filter(|x| x.cmd_seq > q.after && !c.acks.contains_key(&x.cmd_seq))
                            .cloned()
                            .collect(),
                        head,
                    })
                });
            }
        }
    }

    fn model_ack(&self, ctx: &Ctx, seq: u64, body: AckRequest) -> Result<(), ApiError> {
        self.with_conv(ctx, true, |c, _| {
            if !c.commands.iter().any(|x| x.cmd_seq == seq) {
                return Err(not_found("not found"));
            }
            c.acks.insert(seq, body.result);
            Ok(())
        })
    }

    fn model_sent_files(&self, ctx: &Ctx, mut req: SentFilesRequest) -> Result<SentFilesResponse, ApiError> {
        check_tool_use_id(&req.tool_use_id)?;
        if req.files.is_empty() || req.files.len() > SENT_FILES_MAX {
            return Err(ApiError::bad_request("a send has 1-100 files"));
        }
        let mut total = 0u64;
        for f in &mut req.files {
            if let Some(rest) = f.path.strip_prefix("file://") {
                f.path = rest.to_owned();
            }
            if !valid_workspace_path(&f.path) {
                return Err(refuse(
                    ErrorCode::BadPath,
                    "a path is not a normalised path under /workspace",
                ));
            }
            if f.size > SENT_FILE_MAX_BYTES {
                return Err(refuse(ErrorCode::TooLarge, "a file is over 64 MiB"));
            }
            if f.sha256.len() != 64 || !f.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(ApiError::bad_request("sha256 is not 64 hex digits"));
            }
            total += f.size;
        }
        if total > SENT_TOTAL_MAX_BYTES {
            return Err(refuse(ErrorCode::TooLarge, "the send is over 256 MiB"));
        }
        let missing = {
            let blobs = &self.lock().blobs;
            let mut seen = HashSet::new();
            req.files
                .iter()
                .map(|f| f.sha256.clone())
                .filter(|sha| !blobs.contains_key(sha) && seen.insert(sha.clone()))
                .collect::<Vec<_>>()
        };
        self.with_conv(ctx, true, |c, _| {
            if !c.sent.contains_key(&req.tool_use_id) {
                c.sent_staging.insert(req.tool_use_id.clone(), req);
            }
            Ok(())
        })?;
        Ok(SentFilesResponse { missing })
    }

    fn model_sent_files_commit(&self, ctx: &Ctx, tool_use_id: &str) -> Result<SentFilesCommitted, ApiError> {
        check_tool_use_id(tool_use_id)?;
        let id = Self::conversation(ctx)?;
        let mut m = self.lock();
        let Model { convs, blobs, .. } = &mut *m;
        let c = convs.entry(id).or_default();
        let presented = ctx.lease.as_deref().and_then(|l| Uuid::parse_str(l).ok());
        if presented.is_none() || presented != c.lease {
            return Err(lease_lost());
        }
        if let Some(done) = c.sent.get(tool_use_id) {
            return Ok(done.committed);
        }
        let staged = c
            .sent_staging
            .get(tool_use_id)
            .ok_or_else(|| not_found("no sent-files request for this toolUseId"))?;
        if staged.files.iter().any(|f| !blobs.contains_key(&f.sha256)) {
            return Err(ApiError::bad_request("a blob of the send has not been put"));
        }
        let request = c.sent_staging.remove(tool_use_id).unwrap_or_else(|| unreachable!());
        c.sent_seq += 1;
        #[allow(clippy::cast_possible_truncation)] // at most 100 files
        let committed = SentFilesCommitted {
            kept: request.files.len() as u32,
            seq: c.sent_seq,
        };
        c.sent.insert(tool_use_id.to_owned(), SentSend { request, committed });
        Ok(committed)
    }

    fn model_report(&self, ctx: &Ctx) -> Result<(), ApiError> {
        self.with_conv(ctx, true, |_, _| Ok(()))
    }

    fn model_turn_state(&self, ctx: &Ctx, body: &TurnStateReport) -> Result<(), ApiError> {
        self.with_conv(ctx, true, |c, _| {
            if body.state == ReportedState::Running
                && let Some(sid) = &body.provider_session_id
            {
                c.provider_session_id = Some(sid.clone());
                c.main_transcript = Some(
                    body.transcript_path
                        .clone()
                        .unwrap_or_else(|| format!("projects/-workspace/{sid}.jsonl")),
                );
            }
            Ok(())
        })
    }

    fn model_agent_turn(&self, ctx: &Ctx, body: &AgentTurnRequest) -> Result<Admitted<Empty>, ApiError> {
        self.with_conv(ctx, true, |c, _| {
            Ok(if c.agent_turns.insert(body.turn_id) {
                Admitted::New(Empty {})
            } else {
                Admitted::Repeat(Empty {})
            })
        })
    }

    fn model_blob_put(&self, ctx: &Ctx, sha: &str, data: Bytes) -> Result<Admitted<Empty>, ApiError> {
        self.with_conv(ctx, true, |_, _| Ok(()))?;
        if hex_digest(&sha2::Sha256::digest(&data)) != sha {
            return Err(refuse(
                ErrorCode::ShaMismatch,
                "the bytes do not hash to the path's sha256",
            ));
        }
        let mut m = self.lock();
        Ok(if m.blobs.insert(sha.to_owned(), data).is_some() {
            Admitted::Repeat(Empty {})
        } else {
            Admitted::New(Empty {})
        })
    }

    fn model_blob_get(&self, sha: &str) -> Result<Response, ApiError> {
        let data = self
            .lock()
            .blobs
            .get(sha)
            .cloned()
            .ok_or_else(|| not_found("not found"))?;
        let mut res = (StatusCode::OK, data).into_response();
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        Ok(res)
    }
}

pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

async fn bytes_of(body: Body) -> Result<Bytes, ApiError> {
    axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|e| ApiError::bad_request(format!("body: {e}")))
}

/// A scripted-only route: the scripted answer converted with `$conv`, else 501.
macro_rules! scripted {
    ($s:ident, $route:literal, $ctx:expr, $params:expr, $query:expr, $body:expr, $conv:expr) => {
        match $s.record($route, &$ctx, $params, $query, $body, None) {
            Some(r) => $conv(&r, $route),
            None => Err(not_supported($route)),
        }
    };
}

fn typed<T: serde::de::DeserializeOwned>(r: &Reply, route: &str) -> Result<T, ApiError> {
    r.typed(route)
}

fn unit(r: &Reply, _route: &str) -> Result<(), ApiError> {
    r.unit()
}

fn raw(r: &Reply, _route: &str) -> Result<Response, ApiError> {
    r.response()
}

/// M9's rule that magpie checks before anything else: `liveAfter` goes with `liveTurn` (frames are numbered per
/// turn), and both, like `waitMs`, only with `after`. A caller that sends one without the other gets magpie's 400,
/// whatever the test scripted.
fn live_pair(q: &TimelineQuery) -> Result<(), ApiError> {
    if q.after.is_none() && (q.wait_ms.is_some() || q.live_after.is_some() || q.live_turn.is_some()) {
        return Err(ApiError::bad_request("waitMs, liveAfter and liveTurn go with after"));
    }
    if q.live_after.is_some() != q.live_turn.is_some() {
        return Err(ApiError::bad_request("liveAfter and liveTurn go together"));
    }
    Ok(())
}

fn id(v: Uuid) -> Value {
    json!({"id": v.to_string()})
}

#[allow(clippy::too_many_lines, clippy::unused_async_trait_impl)] // every method is async, as the trait
impl Server for FakeMagpie {
    async fn list_conversations(&self, ctx: Ctx, query: ListQuery) -> Result<ListResponse, ApiError> {
        scripted!(self, "list_conversations", ctx, json!({}), ser(&query), None, typed)
    }

    async fn get_conversation(&self, ctx: Ctx, id_: Uuid) -> Result<Conversation, ApiError> {
        scripted!(self, "get_conversation", ctx, id(id_), None, None, typed)
    }

    async fn create_conversation(&self, ctx: Ctx, body: CreateRequest) -> Result<Admitted<CreateResponse>, ApiError> {
        scripted!(self, "create_conversation", ctx, json!({}), None, ser(&body), admitted)
    }

    async fn get_creation(&self, ctx: Ctx, key: String) -> Result<CreateResponse, ApiError> {
        scripted!(
            self,
            "get_creation",
            ctx,
            json!({"idempotencyKey": key}),
            None,
            None,
            typed
        )
    }

    async fn rename_conversation(&self, ctx: Ctx, id_: Uuid, body: RenameRequest) -> Result<Conversation, ApiError> {
        scripted!(self, "rename_conversation", ctx, id(id_), None, ser(&body), typed)
    }

    async fn delete_conversation(&self, ctx: Ctx, id_: Uuid) -> Result<Conversation, ApiError> {
        scripted!(self, "delete_conversation", ctx, id(id_), None, Some(json!({})), typed)
    }

    async fn restore_conversation(&self, ctx: Ctx, id_: Uuid) -> Result<Conversation, ApiError> {
        scripted!(self, "restore_conversation", ctx, id(id_), None, Some(json!({})), typed)
    }

    async fn purge_conversation(&self, ctx: Ctx, id_: Uuid) -> Result<(), ApiError> {
        scripted!(self, "purge_conversation", ctx, id(id_), None, None, unit)
    }

    async fn send_turn(&self, ctx: Ctx, id_: Uuid, body: SendRequest) -> Result<Admitted<SendResponse>, ApiError> {
        scripted!(self, "send_turn", ctx, id(id_), None, ser(&body), admitted)
    }

    async fn cancel_turn(&self, ctx: Ctx, id_: Uuid) -> Result<CancelResponse, ApiError> {
        scripted!(self, "cancel_turn", ctx, id(id_), None, Some(json!({})), typed)
    }

    async fn timeline(&self, ctx: Ctx, id_: Uuid, query: TimelineQuery) -> Result<TimelinePage, ApiError> {
        live_pair(&query)?;
        scripted!(self, "timeline", ctx, id(id_), ser(&query), None, typed)
    }

    async fn live(&self, ctx: Ctx, id_: Uuid) -> Result<Option<LiveState>, ApiError> {
        scripted!(self, "live", ctx, id(id_), None, None, option)
    }

    async fn prompts(&self, ctx: Ctx, id_: Uuid) -> Result<PromptsResponse, ApiError> {
        scripted!(self, "prompts", ctx, id(id_), None, None, typed)
    }

    async fn search(&self, ctx: Ctx, id_: Uuid, query: SearchQuery) -> Result<SearchResponse, ApiError> {
        scripted!(self, "search", ctx, id(id_), ser(&query), None, typed)
    }

    async fn answer_ask(
        &self,
        ctx: Ctx,
        id_: Uuid,
        ask_id: String,
        body: AgentPermissionResponse,
    ) -> Result<(), ApiError> {
        let params = json!({"id": id_.to_string(), "askId": ask_id});
        scripted!(self, "answer_ask", ctx, params, None, ser(&body), unit)
    }

    async fn changes(&self, ctx: Ctx, query: ChangesQuery) -> Result<ChangesResponse, ApiError> {
        scripted!(self, "changes", ctx, json!({}), ser(&query), None, typed)
    }

    async fn upload(&self, ctx: Ctx, meta: UploadMeta, body: Body) -> Result<UploadResponse, ApiError> {
        // the name, kind and content type are in the recorded headers
        let _ = meta;
        let bytes = bytes_of(body).await?;
        match self.record("upload", &ctx, json!({}), None, None, Some(bytes)) {
            Some(r) => r.typed("upload"),
            None => Err(not_supported("upload")),
        }
    }

    async fn files_raw(&self, ctx: Ctx, id_: Uuid, query: RawFileQuery) -> Result<Response, ApiError> {
        scripted!(self, "files_raw", ctx, id(id_), ser(&query), None, raw)
    }

    async fn files_panel(&self, ctx: Ctx, id_: Uuid) -> Result<FilesPanel, ApiError> {
        scripted!(self, "files_panel", ctx, id(id_), None, None, typed)
    }

    async fn blob(&self, ctx: Ctx, blob_id: String, query: BlobQuery) -> Result<Response, ApiError> {
        scripted!(self, "blob", ctx, json!({"blobId": blob_id}), ser(&query), None, raw)
    }

    async fn list_shares(&self, ctx: Ctx, id_: Uuid) -> Result<SharesList, ApiError> {
        scripted!(self, "list_shares", ctx, id(id_), None, None, typed)
    }

    async fn create_share(&self, ctx: Ctx, id_: Uuid) -> Result<ShareRef, ApiError> {
        scripted!(self, "create_share", ctx, id(id_), None, Some(json!({})), typed)
    }

    async fn revoke_share(&self, ctx: Ctx, share_id: String) -> Result<(), ApiError> {
        scripted!(
            self,
            "revoke_share",
            ctx,
            json!({"shareId": share_id}),
            None,
            None,
            unit
        )
    }

    async fn get_share(&self, ctx: Ctx, share_id: String) -> Result<ShareView, ApiError> {
        scripted!(self, "get_share", ctx, json!({"shareId": share_id}), None, None, typed)
    }

    async fn share_files_raw(&self, ctx: Ctx, share_id: String, query: RawFileQuery) -> Result<Response, ApiError> {
        let params = json!({"shareId": share_id});
        scripted!(self, "share_files_raw", ctx, params, ser(&query), None, raw)
    }

    async fn continue_share(&self, ctx: Ctx, share_id: String) -> Result<ContinueResponse, ApiError> {
        let params = json!({"shareId": share_id});
        scripted!(self, "continue_share", ctx, params, None, Some(json!({})), typed)
    }

    async fn view_as(&self, ctx: Ctx, body: ViewAsRequest) -> Result<(), ApiError> {
        scripted!(self, "view_as", ctx, json!({}), None, ser(&body), unit)
    }

    async fn lease(&self, ctx: Ctx) -> Result<LeaseResponse, ApiError> {
        match self.record("lease", &ctx, json!({}), None, Some(json!({})), None) {
            Some(r) => r.typed("lease"),
            None => self.model_lease(&ctx),
        }
    }

    async fn cc_tree(&self, ctx: Ctx) -> Result<CcTree, ApiError> {
        match self.record("cc_tree", &ctx, json!({}), None, None, None) {
            Some(r) => r.typed("cc_tree"),
            None => self.model_tree(&ctx),
        }
    }

    async fn cc_file(&self, ctx: Ctx, query: CcFileQuery) -> Result<Response, ApiError> {
        match self.record("cc_file", &ctx, json!({}), ser(&query), None, None) {
            Some(r) => r.response(),
            None => self.model_file(&ctx, &query),
        }
    }

    async fn cc_write(&self, ctx: Ctx, query: CcWriteQuery, body: Body) -> Result<CcSizeVersion, ApiError> {
        let bytes = bytes_of(body).await?;
        match self.record("cc_write", &ctx, json!({}), ser(&query), None, Some(bytes.clone())) {
            Some(r) => r.typed("cc_write"),
            None => self.model_write(&ctx, &query, &bytes),
        }
    }

    async fn cc_truncate(&self, ctx: Ctx, body: CcTruncate) -> Result<CcSizeVersion, ApiError> {
        match self.record("cc_truncate", &ctx, json!({}), None, ser(&body), None) {
            Some(r) => r.typed("cc_truncate"),
            None => self.model_truncate(&ctx, &body),
        }
    }

    async fn cc_rename(&self, ctx: Ctx, body: CcRename) -> Result<CcVersion, ApiError> {
        match self.record("cc_rename", &ctx, json!({}), None, ser(&body), None) {
            Some(r) => r.typed("cc_rename"),
            None => self.model_rename(&ctx, &body),
        }
    }

    async fn cc_unlink(&self, ctx: Ctx, body: CcPathOp) -> Result<(), ApiError> {
        match self.record("cc_unlink", &ctx, json!({}), None, ser(&body), None) {
            Some(r) => r.unit(),
            None => self.model_unlink(&ctx, &body),
        }
    }

    async fn cc_mkdir(&self, ctx: Ctx, body: CcMkdir) -> Result<(), ApiError> {
        match self.record("cc_mkdir", &ctx, json!({}), None, ser(&body), None) {
            Some(r) => r.unit(),
            None => self.model_mkdir(&ctx, &body),
        }
    }

    async fn cc_rmdir(&self, ctx: Ctx, body: CcPathOp) -> Result<(), ApiError> {
        match self.record("cc_rmdir", &ctx, json!({}), None, ser(&body), None) {
            Some(r) => r.unit(),
            None => self.model_rmdir(&ctx, &body),
        }
    }

    async fn cc_setattr(&self, ctx: Ctx, body: CcSetattr) -> Result<CcVersion, ApiError> {
        match self.record("cc_setattr", &ctx, json!({}), None, ser(&body), None) {
            Some(r) => r.typed("cc_setattr"),
            None => self.model_setattr(&ctx, &body),
        }
    }

    async fn commands(&self, ctx: Ctx, query: CommandsQuery) -> Result<CommandsResponse, ApiError> {
        match self.record("commands", &ctx, json!({}), ser(&query), None, None) {
            Some(r) => r.typed("commands"),
            None => self.model_commands(&ctx, query).await,
        }
    }

    async fn ack_command(&self, ctx: Ctx, cmd_seq: u64, body: AckRequest) -> Result<(), ApiError> {
        let params = json!({"cmdSeq": cmd_seq.to_string()});
        match self.record("ack_command", &ctx, params, None, ser(&body), None) {
            Some(r) => r.unit(),
            None => self.model_ack(&ctx, cmd_seq, body),
        }
    }

    async fn agent_turn(&self, ctx: Ctx, body: AgentTurnRequest) -> Result<Admitted<Empty>, ApiError> {
        match self.record("agent_turn", &ctx, json!({}), None, ser(&body), None) {
            Some(r) => admitted(&r, "agent_turn"),
            None => self.model_agent_turn(&ctx, &body),
        }
    }

    async fn turn_state(&self, ctx: Ctx, turn_id: Uuid, body: TurnStateReport) -> Result<(), ApiError> {
        let params = json!({"turnId": turn_id.to_string()});
        match self.record("turn_state", &ctx, params, None, ser(&body), None) {
            Some(r) => r.unit(),
            None => self.model_turn_state(&ctx, &body),
        }
    }

    async fn live_frames(&self, ctx: Ctx, turn_id: Uuid, body: LiveFramesRequest) -> Result<(), ApiError> {
        let params = json!({"turnId": turn_id.to_string()});
        match self.record("live_frames", &ctx, params, None, ser(&body), None) {
            Some(r) => r.unit(),
            None => self.model_report(&ctx),
        }
    }

    async fn tasks(&self, ctx: Ctx, body: TasksReport) -> Result<(), ApiError> {
        match self.record("tasks", &ctx, json!({}), None, ser(&body), None) {
            Some(r) => r.unit(),
            None => self.model_report(&ctx),
        }
    }

    async fn ask_open(&self, ctx: Ctx, body: AskReport) -> Result<(), ApiError> {
        match self.record("ask_open", &ctx, json!({}), None, ser(&body), None) {
            Some(r) => r.unit(),
            None => self.model_report(&ctx),
        }
    }

    async fn ask_resolved(&self, ctx: Ctx, ask_id: String, body: AskResolvedReport) -> Result<(), ApiError> {
        match self.record("ask_resolved", &ctx, json!({"askId": ask_id}), None, ser(&body), None) {
            Some(r) => r.unit(),
            None => self.model_report(&ctx),
        }
    }

    async fn heartbeat(&self, ctx: Ctx, body: Heartbeat) -> Result<(), ApiError> {
        match self.record("heartbeat", &ctx, json!({}), None, ser(&body), None) {
            Some(r) => r.unit(),
            None => self.model_report(&ctx),
        }
    }

    async fn sandbox_blob_get(&self, ctx: Ctx, sha: String, query: SandboxBlobQuery) -> Result<Response, ApiError> {
        match self.record("sandbox_blob_get", &ctx, json!({"sha": sha}), ser(&query), None, None) {
            Some(r) => r.response(),
            None => self.model_blob_get(&sha),
        }
    }

    async fn sandbox_blob_put(&self, ctx: Ctx, sha: String, body: Body) -> Result<Admitted<Empty>, ApiError> {
        let bytes = bytes_of(body).await?;
        match self.record(
            "sandbox_blob_put",
            &ctx,
            json!({"sha": sha}),
            None,
            None,
            Some(bytes.clone()),
        ) {
            Some(r) => admitted(&r, "sandbox_blob_put"),
            None => self.model_blob_put(&ctx, &sha, bytes),
        }
    }

    async fn sent_files(&self, ctx: Ctx, body: SentFilesRequest) -> Result<SentFilesResponse, ApiError> {
        match self.record("sent_files", &ctx, json!({}), None, ser(&body), None) {
            Some(r) => r.typed("sent_files"),
            None => self.model_sent_files(&ctx, body),
        }
    }

    async fn sent_files_commit(&self, ctx: Ctx, tool_use_id: String) -> Result<SentFilesCommitted, ApiError> {
        let params = json!({"toolUseId": tool_use_id});
        match self.record("sent_files_commit", &ctx, params, None, Some(json!({})), None) {
            Some(r) => r.typed("sent_files_commit"),
            None => self.model_sent_files_commit(&ctx, &tool_use_id),
        }
    }

    async fn person_purge(
        &self,
        ctx: Ctx,
        person: String,
        body: PersonPurgeRequest,
    ) -> Result<PersonPurgeResponse, ApiError> {
        scripted!(
            self,
            "person_purge",
            ctx,
            json!({"person": person}),
            None,
            ser(&body),
            typed
        )
    }

    async fn workspaces_exists(
        &self,
        ctx: Ctx,
        body: WorkspacesExistsRequest,
    ) -> Result<WorkspacesExistsResponse, ApiError> {
        scripted!(self, "workspaces_exists", ctx, json!({}), None, ser(&body), typed)
    }

    async fn legacy_import(&self, ctx: Ctx) -> Result<LegacyImportStatus, ApiError> {
        scripted!(self, "legacy_import", ctx, json!({}), None, None, typed)
    }

    async fn ops_people(&self, ctx: Ctx, query: PeopleQuery) -> Result<PeopleResponse, ApiError> {
        scripted!(self, "ops_people", ctx, json!({}), ser(&query), None, typed)
    }

    async fn ops_view_as(&self, ctx: Ctx, query: ViewAsLogQuery) -> Result<ViewAsLog, ApiError> {
        scripted!(self, "ops_view_as", ctx, json!({}), ser(&query), None, typed)
    }

    async fn ops_stats(&self, ctx: Ctx) -> Result<OpsStats, ApiError> {
        scripted!(self, "ops_stats", ctx, json!({}), None, None, typed)
    }

    async fn admin_conversations(&self, ctx: Ctx, query: AdminListQuery) -> Result<AdminConversationList, ApiError> {
        scripted!(self, "admin_conversations", ctx, json!({}), ser(&query), None, typed)
    }

    async fn admin_conversation(&self, ctx: Ctx, id_: Uuid) -> Result<AdminConversation, ApiError> {
        scripted!(self, "admin_conversation", ctx, id(id_), None, None, typed)
    }

    async fn admin_events(&self, ctx: Ctx, id_: Uuid, query: EventsQuery) -> Result<EventsPage, ApiError> {
        scripted!(self, "admin_events", ctx, id(id_), ser(&query), None, typed)
    }

    async fn admin_timeline(&self, ctx: Ctx, id_: Uuid, query: TimelineQuery) -> Result<TimelinePage, ApiError> {
        live_pair(&query)?;
        scripted!(self, "admin_timeline", ctx, id(id_), ser(&query), None, typed)
    }

    async fn legacy_ticket(&self, ctx: Ctx, id_: Uuid, body: TicketRequest) -> Result<TicketResponse, ApiError> {
        scripted!(self, "legacy_ticket", ctx, id(id_), None, ser(&body), typed)
    }

    async fn legacy_ticket_done(&self, ctx: Ctx, ticket: Uuid, body: TicketDone) -> Result<(), ApiError> {
        scripted!(
            self,
            "legacy_ticket_done",
            ctx,
            json!({"ticket": ticket.to_string()}),
            None,
            ser(&body),
            unit
        )
    }

    async fn legacy_ticket_claim(&self, ctx: Ctx, ticket: Uuid, body: TicketClaim) -> Result<TicketClaimed, ApiError> {
        scripted!(
            self,
            "legacy_ticket_claim",
            ctx,
            json!({"ticket": ticket.to_string()}),
            None,
            ser(&body),
            typed
        )
    }

    async fn upload_legacy_path(&self, ctx: Ctx, upload_id: Uuid, body: LegacyPathRequest) -> Result<(), ApiError> {
        scripted!(
            self,
            "upload_legacy_path",
            ctx,
            json!({"uploadId": upload_id.to_string()}),
            None,
            ser(&body),
            unit
        )
    }

    async fn legacy_window(&self, ctx: Ctx) -> Result<WindowState, ApiError> {
        // unscripted: a deployment outside the switch window (nothing legacy), which every pigeon upload reads
        match self.record("legacy_window", &ctx, json!({}), None, None, None) {
            Some(r) => typed(&r, "legacy_window"),
            None => Ok(WindowState::closed()),
        }
    }

    async fn upload_raw(&self, ctx: Ctx, upload_id: Uuid) -> Result<Response, ApiError> {
        scripted!(
            self,
            "upload_raw",
            ctx,
            json!({"uploadId": upload_id.to_string()}),
            None,
            None,
            raw
        )
    }
}
