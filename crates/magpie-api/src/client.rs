//! The typed client every caller of magpie uses (feature `client`).
//!
//! ```no_run
//! # async fn demo(http: reqwest::Client) -> Result<(), magpie_api::client::ClientError> {
//! use magpie_api::client::Client;
//! let magpie = Client::new(http, "http://magpie", "<pair token>");
//! let page = magpie.person("a@example.com").list_conversations(&Default::default()).await?;
//! # let _ = page; Ok(()) }
//! ```
//!
//! The caller builds the `reqwest::Client` (aviary-core's builder sets the pool, timeouts and `User-Agent`); this
//! client adds the bearer token, `Aviary-Request` (a fresh uuid v7 unless one is forwarded), `Aviary-Protocol` for the
//! route's surface and the identity headers of the scope ([`PersonCalls`], [`SandboxCalls`], [`ServiceCalls`]).
//! A status outside the route's success list is an error: the flat envelope as [`ClientError::Api`].

use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::changes::{ChangesQuery, ChangesResponse};
use crate::conversations::{Conversation, CreateRequest, CreateResponse, ListQuery, ListResponse, RenameRequest};
use crate::errors::{ErrorCode, Refusal};
use crate::events::{EventsPage, EventsQuery};
use crate::files::{FilesPanel, RawFileQuery};
use crate::headers::{
    AVIARY_ADMIN, AVIARY_LEASE, AVIARY_PERSON, AVIARY_PROTOCOL, AVIARY_REQUEST, AVIARY_UPLOAD_KIND, AVIARY_UPLOAD_NAME,
    AVIARY_VIEW_SESSION, AVIARY_VIEWER,
};
use crate::legacy::LegacyPathRequest;
use crate::live::{LiveFramesRequest, LiveState};
use crate::ops::{
    AdminConversation, AdminConversationList, AdminListQuery, HermitCreateRequest, HermitSendRequest,
    LegacyImportStatus, OpsStats, PeopleQuery, PeopleResponse, PersonPurgeRequest, PersonPurgeResponse, ViewAsLog,
    ViewAsLogQuery, ViewAsRequest, WorkspacesExistsRequest, WorkspacesExistsResponse,
};
use crate::routes::{self, Route, Surface};
use crate::rows::{
    PromptsResponse, SearchQuery, SearchResponse, TimelinePage, TimelineQuery, TranscriptPage, TranscriptQuery,
};
use crate::sandbox::{
    AckRequest, AgentTurnRequest, AskReport, AskResolvedReport, CcFileQuery, CcMkdir, CcPathOp, CcRename, CcSetattr,
    CcSizeVersion, CcTree, CcTruncate, CcVersion, CcWriteQuery, CommandsQuery, CommandsResponse, Heartbeat,
    LeaseResponse, SentFilesCommitted, SentFilesRequest, SentFilesResponse, TasksReport, TurnStateReport,
};
use crate::schedules::{AdminSchedules, CreateSchedule, PauseAll, Schedule, SchedulesList, UpdateSchedule};
use crate::shares::{ContinueResponse, ShareRef, ShareView, SharesList};
use crate::turns::{AgentPermissionResponse, CancelResponse, SendRequest, SendResponse};
use crate::uploads::{BlobQuery, SandboxBlobQuery, UploadKind, UploadResponse};
use crate::{Admitted, Empty, PROTOCOL_PERSON, PROTOCOL_SANDBOX, SURFACE_PERSON, SURFACE_SANDBOX};

/// The request headers a byte relay forwards to magpie's byte routes (M14, M15, M16 download): the range and the
/// conditional headers of the app's request, so magpie answers 206, 304 or 416 itself (pigeon 5.8).
pub const RELAY_REQUEST_HEADERS: [reqwest::header::HeaderName; 4] = [
    reqwest::header::RANGE,
    reqwest::header::IF_NONE_MATCH,
    reqwest::header::IF_MODIFIED_SINCE,
    reqwest::header::IF_RANGE,
];

/// A failed call.
#[derive(Debug)]
pub enum ClientError {
    /// magpie answered with the error envelope (or a status outside the route's success list).
    Api(Refusal),
    /// The request did not complete (connection, timeout, body stream).
    Http(reqwest::Error),
    /// A 2xx body that does not parse as the route's response type.
    Decode {
        /// The route.
        route: &'static str,
        /// The parse error.
        error: String,
    },
}

impl ClientError {
    /// The envelope, if magpie answered with one.
    #[must_use]
    pub fn api(&self) -> Option<&Refusal> {
        match self {
            ClientError::Api(e) => Some(e),
            _ => None,
        }
    }

    /// The error code, if magpie answered with one.
    #[must_use]
    pub fn code(&self) -> Option<&ErrorCode> {
        self.api().map(|e| &e.code)
    }

    /// A failure that a retry may cure: no answer, or 502/503/504.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            ClientError::Http(_) => true,
            ClientError::Api(e) => matches!(e.status, 502..=504),
            ClientError::Decode { .. } => false,
        }
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Api(e) => write!(f, "magpie: {e}"),
            ClientError::Http(e) => write!(f, "magpie unreachable: {e}"),
            ClientError::Decode { route, error } => write!(f, "magpie {route}: bad response body: {error}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<reqwest::Error> for ClientError {
    fn from(e: reqwest::Error) -> Self {
        ClientError::Http(e)
    }
}

/// magpie's client: the base URL (`http://magpie`) and the caller's pair token.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

/// Headers a scope adds to its calls.
#[derive(Debug, Clone, Default)]
struct Scope {
    request_id: Option<String>,
    person: Option<String>,
    viewer: Option<(String, String)>,
    admin: Option<String>,
    lease: Option<Uuid>,
}

enum Body {
    None,
    Json(Vec<u8>),
    Raw(reqwest::Body, String),
}

impl Client {
    /// A client for `base` (no trailing slash needed) presenting `token`.
    pub fn new(http: reqwest::Client, base: impl Into<String>, token: impl Into<String>) -> Self {
        let base = base.into().trim_end_matches('/').to_owned();
        Client {
            http,
            base,
            token: token.into(),
        }
    }

    /// pigeon's calls for a signed-in person.
    #[must_use]
    pub fn person(&self, person: &str) -> PersonCalls<'_> {
        PersonCalls {
            client: self,
            scope: Scope {
                person: Some(person.to_owned()),
                ..Scope::default()
            },
        }
    }

    /// pigeon's calls while `viewer` (an admin) views as `person` in view session `session`.
    #[must_use]
    pub fn viewing_as(&self, person: &str, viewer: &str, session: &str) -> PersonCalls<'_> {
        PersonCalls {
            client: self,
            scope: Scope {
                person: Some(person.to_owned()),
                viewer: Some((viewer.to_owned(), session.to_owned())),
                ..Scope::default()
            },
        }
    }

    /// The supervisor's calls (through the gateway, which adds the sandbox identity).
    #[must_use]
    pub fn sandbox(&self, lease: Option<Uuid>) -> SandboxCalls<'_> {
        SandboxCalls {
            client: self,
            scope: Scope {
                lease,
                ..Scope::default()
            },
        }
    }

    /// hermit's and the ops tools' calls; `admin` is the operator behind admin routes (`Aviary-Admin`).
    #[must_use]
    pub fn service(&self, admin: Option<&str>) -> ServiceCalls<'_> {
        ServiceCalls {
            client: self,
            scope: Scope {
                admin: admin.map(str::to_owned),
                ..Scope::default()
            },
        }
    }

    /// M15 `GET /v1/blobs/{blobId}` (no person header: pigeon checked the signed URL). The response streams.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn blob(
        &self,
        blob_id: &str,
        q: &BlobQuery,
        range: Option<&str>,
    ) -> Result<reqwest::Response, ClientError> {
        let r = route("blob");
        self.exec(&Scope::default(), r, &[blob_id], Some(q), Body::None, range)
            .await
    }

    /// M17 `POST /v1/view-as`: record a view-as start; pigeon opens the view only after this succeeds.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn view_as(&self, req: &ViewAsRequest) -> Result<(), ClientError> {
        let r = route("view_as");
        self.exec(&Scope::default(), r, &[], None::<&Empty>, json(req)?, None)
            .await
            .map(drop)
    }

    /// M15 for a byte relay (pigeon's `/api/blobs/…`): `forward` holds the client's request headers of
    /// [`RELAY_REQUEST_HEADERS`] (others are ignored), and the answer comes back whatever its status (200, 304, 404
    /// …) for the caller to pass on; only a transport failure is an error.
    ///
    /// # Errors
    /// [`ClientError::Http`].
    pub async fn blob_relay(
        &self,
        blob_id: &str,
        q: &BlobQuery,
        forward: &reqwest::header::HeaderMap,
    ) -> Result<reqwest::Response, ClientError> {
        let r = route("blob");
        self.relay(&Scope::default(), r, &[blob_id], q, forward).await
    }

    async fn relay<Q: Serialize + ?Sized>(
        &self,
        scope: &Scope,
        route: &'static Route,
        params: &[&str],
        query: &Q,
        forward: &reqwest::header::HeaderMap,
    ) -> Result<reqwest::Response, ClientError> {
        let mut b = self.request(scope, route, params, Some(query));
        for name in &RELAY_REQUEST_HEADERS {
            for v in forward.get_all(name) {
                b = b.header(name.clone(), v.clone());
            }
        }
        Ok(b.send().await?)
    }

    fn request<Q: Serialize + ?Sized>(
        &self,
        scope: &Scope,
        route: &'static Route,
        params: &[&str],
        query: Option<&Q>,
    ) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base, routes::fill(route.path, params));
        let method = match route.method {
            routes::Method::Get => reqwest::Method::GET,
            routes::Method::Post => reqwest::Method::POST,
            routes::Method::Put => reqwest::Method::PUT,
            routes::Method::Patch => reqwest::Method::PATCH,
            routes::Method::Delete => reqwest::Method::DELETE,
        };
        let protocol = match route.surface {
            Surface::Person => format!("{SURFACE_PERSON}={PROTOCOL_PERSON}"),
            Surface::Sandbox => format!("{SURFACE_SANDBOX}={PROTOCOL_SANDBOX}"),
        };
        let request_id = scope.request_id.clone().unwrap_or_else(|| Uuid::now_v7().to_string());
        let mut b = self
            .http
            .request(method, url)
            .bearer_auth(&self.token)
            .header(AVIARY_REQUEST, request_id)
            .header(AVIARY_PROTOCOL, protocol);
        if let Some(q) = query {
            b = b.query(q);
        }
        if let Some(p) = &scope.person {
            b = b.header(AVIARY_PERSON, p);
        }
        if let Some((viewer, session)) = &scope.viewer {
            b = b.header(AVIARY_VIEWER, viewer).header(AVIARY_VIEW_SESSION, session);
        }
        if let Some(a) = &scope.admin {
            b = b.header(AVIARY_ADMIN, a);
        }
        if let Some(l) = &scope.lease {
            b = b.header(AVIARY_LEASE, l.to_string());
        }
        b
    }

    async fn exec<Q: Serialize + ?Sized>(
        &self,
        scope: &Scope,
        route: &'static Route,
        params: &[&str],
        query: Option<&Q>,
        body: Body,
        range: Option<&str>,
    ) -> Result<reqwest::Response, ClientError> {
        let mut b = self.request(scope, route, params, query);
        if let Some(range) = range {
            b = b.header(reqwest::header::RANGE, range);
        }
        b = match body {
            Body::None => b,
            Body::Json(bytes) => b.header(reqwest::header::CONTENT_TYPE, "application/json").body(bytes),
            Body::Raw(body, content_type) => b.header(reqwest::header::CONTENT_TYPE, content_type).body(body),
        };
        let resp = b.send().await?;
        let status = resp.status().as_u16();
        if route.ok.contains(&status) {
            return Ok(resp);
        }
        Err(ClientError::Api(refusal(resp).await?))
    }

    async fn exec_json<Q: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        scope: &Scope,
        route: &'static Route,
        params: &[&str],
        query: Option<&Q>,
        body: Body,
    ) -> Result<T, ClientError> {
        let resp = self.exec(scope, route, params, query, body, None).await?;
        decode(route, resp).await
    }

    async fn exec_admitted<Q: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        scope: &Scope,
        route: &'static Route,
        params: &[&str],
        query: Option<&Q>,
        body: Body,
    ) -> Result<Admitted<T>, ClientError> {
        let resp = self.exec(scope, route, params, query, body, None).await?;
        let repeat = resp.status().as_u16() == 200;
        let t = decode(route, resp).await?;
        Ok(if repeat { Admitted::Repeat(t) } else { Admitted::New(t) })
    }
}

/// Read an answer outside the route's success list as a [`Refusal`]: the envelope, or one made from the status when
/// the body is not an envelope (a proxy's page).
async fn refusal(resp: reqwest::Response) -> Result<Refusal, ClientError> {
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok());
    let bytes = resp.bytes().await?;
    let mut r = serde_json::from_slice::<Refusal>(&bytes).unwrap_or_else(|_| {
        let code = if status >= 500 {
            ErrorCode::Unavailable
        } else {
            ErrorCode::BadRequest
        };
        Refusal::new(code, format!("status {status}: {}", String::from_utf8_lossy(&bytes)))
    });
    r.status = status;
    r.retry_after = retry_after;
    Ok(r)
}

fn route(id: &str) -> &'static Route {
    // every id used here is in ROUTES (tests call every method)
    routes::by_id(id).unwrap_or(&routes::ROUTES[0])
}

fn json<T: Serialize + ?Sized>(t: &T) -> Result<Body, ClientError> {
    serde_json::to_vec(t).map(Body::Json).map_err(|e| ClientError::Decode {
        route: "request",
        error: e.to_string(),
    })
}

async fn decode<T: DeserializeOwned>(route: &'static Route, resp: reqwest::Response) -> Result<T, ClientError> {
    let bytes = resp.bytes().await?;
    serde_json::from_slice(&bytes).map_err(|e| ClientError::Decode {
        route: route.id,
        error: e.to_string(),
    })
}

const NO_QUERY: Option<&Empty> = None;

macro_rules! forward_request_id {
    () => {
        /// Forward this correlation id instead of minting one.
        #[must_use]
        pub fn with_request_id(mut self, id: impl Into<String>) -> Self {
            self.scope.request_id = Some(id.into());
            self
        }
    };
}

/// pigeon's person-scoped calls (16.1). Made by [`Client::person`] or [`Client::viewing_as`].
#[derive(Debug, Clone)]
pub struct PersonCalls<'a> {
    client: &'a Client,
    scope: Scope,
}

#[allow(clippy::missing_errors_doc)] // every method: ClientError
impl PersonCalls<'_> {
    forward_request_id!();

    async fn get<Q: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        id: &str,
        params: &[&str],
        q: Option<&Q>,
    ) -> Result<T, ClientError> {
        self.client
            .exec_json(&self.scope, route(id), params, q, Body::None)
            .await
    }

    async fn send_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        id: &str,
        params: &[&str],
        body: &B,
    ) -> Result<T, ClientError> {
        self.client
            .exec_json(&self.scope, route(id), params, NO_QUERY, json(body)?)
            .await
    }

    async fn empty(&self, id: &str, params: &[&str], body: Body) -> Result<(), ClientError> {
        self.client
            .exec(&self.scope, route(id), params, NO_QUERY, body, None)
            .await
            .map(drop)
    }

    /// M1.
    pub async fn list_conversations(&self, q: &ListQuery) -> Result<ListResponse, ClientError> {
        self.get("list_conversations", &[], Some(q)).await
    }

    /// M2.
    pub async fn get_conversation(&self, id: Uuid) -> Result<Conversation, ClientError> {
        self.get("get_conversation", &[&id.to_string()], NO_QUERY).await
    }

    /// M3.
    pub async fn create_conversation(&self, req: &CreateRequest) -> Result<Admitted<CreateResponse>, ClientError> {
        self.client
            .exec_admitted(&self.scope, route("create_conversation"), &[], NO_QUERY, json(req)?)
            .await
    }

    /// M4.
    pub async fn get_creation(&self, idempotency_key: &str) -> Result<CreateResponse, ClientError> {
        self.get("get_creation", &[idempotency_key], NO_QUERY).await
    }

    /// M5.
    pub async fn rename_conversation(&self, id: Uuid, req: &RenameRequest) -> Result<Conversation, ClientError> {
        self.send_json("rename_conversation", &[&id.to_string()], req).await
    }

    /// M6 soft delete.
    pub async fn delete_conversation(&self, id: Uuid) -> Result<Conversation, ClientError> {
        self.send_json("delete_conversation", &[&id.to_string()], &Empty {})
            .await
    }

    /// M6 restore.
    pub async fn restore_conversation(&self, id: Uuid) -> Result<Conversation, ClientError> {
        self.send_json("restore_conversation", &[&id.to_string()], &Empty {})
            .await
    }

    /// M6 permanent delete (only from Recently deleted).
    pub async fn purge_conversation(&self, id: Uuid) -> Result<(), ClientError> {
        self.empty("purge_conversation", &[&id.to_string()], Body::None).await
    }

    /// M7.
    pub async fn send_turn(&self, id: Uuid, req: &SendRequest) -> Result<Admitted<SendResponse>, ClientError> {
        self.client
            .exec_admitted(
                &self.scope,
                route("send_turn"),
                &[&id.to_string()],
                NO_QUERY,
                json(req)?,
            )
            .await
    }

    /// M8.
    pub async fn cancel_turn(&self, id: Uuid) -> Result<CancelResponse, ClientError> {
        self.send_json("cancel_turn", &[&id.to_string()], &Empty {}).await
    }

    /// M9.
    pub async fn timeline(&self, id: Uuid, q: &TimelineQuery) -> Result<TimelinePage, ClientError> {
        self.get("timeline", &[&id.to_string()], Some(q)).await
    }

    /// The running turn's live state; `None` (204) when no turn is active.
    pub async fn live(&self, id: Uuid) -> Result<Option<LiveState>, ClientError> {
        let r = route("live");
        let resp = self
            .client
            .exec(&self.scope, r, &[&id.to_string()], NO_QUERY, Body::None, None)
            .await?;
        if resp.status().as_u16() == 204 {
            return Ok(None);
        }
        decode(r, resp).await.map(Some)
    }

    /// M10.
    pub async fn prompts(&self, id: Uuid) -> Result<PromptsResponse, ClientError> {
        self.get("prompts", &[&id.to_string()], NO_QUERY).await
    }

    /// Search within the conversation.
    pub async fn search(&self, id: Uuid, q: &SearchQuery) -> Result<SearchResponse, ClientError> {
        self.get("search", &[&id.to_string()], Some(q)).await
    }

    /// The main transcript's lines (the raw view; only while viewing as).
    pub async fn transcript(&self, id: Uuid, q: &TranscriptQuery) -> Result<TranscriptPage, ClientError> {
        self.get("transcript", &[&id.to_string()], Some(q)).await
    }

    /// M11.
    pub async fn answer_ask(
        &self,
        id: Uuid,
        ask_id: &str,
        answer: &AgentPermissionResponse,
    ) -> Result<(), ClientError> {
        self.empty("answer_ask", &[&id.to_string(), ask_id], json(answer)?)
            .await
    }

    /// M12.
    pub async fn changes(&self, q: &ChangesQuery) -> Result<ChangesResponse, ClientError> {
        self.get("changes", &[], Some(q)).await
    }

    /// M13: `body` streams to magpie; `name` is percent-encoded into `Aviary-Upload-Name`.
    pub async fn upload(
        &self,
        name: &str,
        kind: UploadKind,
        content_type: &str,
        body: impl Into<reqwest::Body>,
    ) -> Result<UploadResponse, ClientError> {
        let r = route("upload");
        let url = format!("{}{}", self.client.base, r.path);
        let request_id = self
            .scope
            .request_id
            .clone()
            .unwrap_or_else(|| Uuid::now_v7().to_string());
        let mut b = self
            .client
            .http
            .post(url)
            .bearer_auth(&self.client.token)
            .header(AVIARY_REQUEST, request_id)
            .header(AVIARY_PROTOCOL, format!("{SURFACE_PERSON}={PROTOCOL_PERSON}"))
            .header(AVIARY_UPLOAD_NAME, routes::encode_segment(name))
            .header(AVIARY_UPLOAD_KIND, kind.as_str())
            .header(reqwest::header::CONTENT_TYPE, content_type);
        if let Some(p) = &self.scope.person {
            b = b.header(AVIARY_PERSON, p);
        }
        if let Some((viewer, session)) = &self.scope.viewer {
            b = b.header(AVIARY_VIEWER, viewer).header(AVIARY_VIEW_SESSION, session);
        }
        let resp = b.body(body).send().await?;
        let status = resp.status().as_u16();
        if !r.ok.contains(&status) {
            return Err(ClientError::Api(refusal(resp).await?));
        }
        decode(r, resp).await
    }

    /// M14: a workspace file or a sent-file snapshot. The response streams; `range` is passed as `Range`.
    pub async fn files_raw(
        &self,
        id: Uuid,
        q: &RawFileQuery,
        range: Option<&str>,
    ) -> Result<reqwest::Response, ClientError> {
        self.client
            .exec(
                &self.scope,
                route("files_raw"),
                &[&id.to_string()],
                Some(q),
                Body::None,
                range,
            )
            .await
    }

    /// M14 for a byte relay: as [`Client::blob_relay`], the answer whatever its status.
    pub async fn files_raw_relay(
        &self,
        id: Uuid,
        q: &RawFileQuery,
        forward: &reqwest::header::HeaderMap,
    ) -> Result<reqwest::Response, ClientError> {
        self.client
            .relay(&self.scope, route("files_raw"), &[&id.to_string()], q, forward)
            .await
    }

    /// The files panel.
    pub async fn files_panel(&self, id: Uuid) -> Result<FilesPanel, ClientError> {
        self.get("files_panel", &[&id.to_string()], NO_QUERY).await
    }

    /// The conversation's schedules with their last runs (the app's panel).
    /// Takes back a message that waits for the running turn to end (409 `bad_transition` once it is on its way).
    pub async fn withdraw_pending(&self, id: Uuid, client_message_id: &str) -> Result<Conversation, ClientError> {
        self.client
            .exec_json(
                &self.scope,
                route("withdraw_pending"),
                &[&id.to_string(), client_message_id],
                NO_QUERY,
                Body::None,
            )
            .await
    }

    /// The slash commands the conversation's Claude Code names.
    pub async fn conversation_commands(&self, id: Uuid) -> Result<crate::sandbox::CommandsList, ClientError> {
        self.get("conversation_commands", &[&id.to_string()], NO_QUERY).await
    }

    /// The conversation's tasks (the app's activity panel).
    pub async fn conversation_tasks(&self, id: Uuid) -> Result<crate::tasks::TasksList, ClientError> {
        self.get("conversation_tasks", &[&id.to_string()], NO_QUERY).await
    }

    /// Stops one of them (202: Claude Code is told to; nothing happens when it no longer runs).
    pub async fn stop_task(&self, id: Uuid, task_id: &str) -> Result<(), ClientError> {
        self.empty("stop_task", &[&id.to_string(), task_id], json(&Empty {})?)
            .await
    }

    pub async fn conversation_schedules(&self, id: Uuid) -> Result<SchedulesList, ClientError> {
        self.get("conversation_schedules", &[&id.to_string()], NO_QUERY).await
    }

    /// M16 list.
    pub async fn list_shares(&self, id: Uuid) -> Result<SharesList, ClientError> {
        self.get("list_shares", &[&id.to_string()], NO_QUERY).await
    }

    /// M16 create.
    pub async fn create_share(&self, id: Uuid) -> Result<ShareRef, ClientError> {
        self.send_json("create_share", &[&id.to_string()], &Empty {}).await
    }

    /// M16 revoke.
    pub async fn revoke_share(&self, share_id: &str) -> Result<(), ClientError> {
        self.empty("revoke_share", &[share_id], Body::None).await
    }

    /// M16 view.
    pub async fn get_share(&self, share_id: &str) -> Result<ShareView, ClientError> {
        self.get("get_share", &[share_id], NO_QUERY).await
    }

    /// M16 download.
    pub async fn share_files_raw(
        &self,
        share_id: &str,
        q: &RawFileQuery,
        range: Option<&str>,
    ) -> Result<reqwest::Response, ClientError> {
        self.client
            .exec(
                &self.scope,
                route("share_files_raw"),
                &[share_id],
                Some(q),
                Body::None,
                range,
            )
            .await
    }

    /// M16 download for a byte relay: as [`Client::blob_relay`], the answer whatever its status.
    pub async fn share_files_raw_relay(
        &self,
        share_id: &str,
        q: &RawFileQuery,
        forward: &reqwest::header::HeaderMap,
    ) -> Result<reqwest::Response, ClientError> {
        self.client
            .relay(&self.scope, route("share_files_raw"), &[share_id], q, forward)
            .await
    }

    /// M16 continue.
    pub async fn continue_share(&self, share_id: &str) -> Result<ContinueResponse, ClientError> {
        self.send_json("continue_share", &[share_id], &Empty {}).await
    }

    /// Until A7: the old asset path a teed upload got from the old child.
    pub async fn upload_legacy_path(&self, upload_id: Uuid, req: &LegacyPathRequest) -> Result<(), ClientError> {
        self.empty("upload_legacy_path", &[&upload_id.to_string()], json(req)?)
            .await
    }
}

/// The supervisor's calls (16.2). Made by [`Client::sandbox`]; every state change carries the lease.
#[derive(Debug, Clone)]
pub struct SandboxCalls<'a> {
    client: &'a Client,
    scope: Scope,
}

#[allow(clippy::missing_errors_doc)] // every method: ClientError
impl SandboxCalls<'_> {
    forward_request_id!();

    /// The same calls with another lease.
    #[must_use]
    pub fn with_lease(mut self, lease: Uuid) -> Self {
        self.scope.lease = Some(lease);
        self
    }

    async fn post<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        id: &str,
        params: &[&str],
        body: &B,
    ) -> Result<T, ClientError> {
        self.client
            .exec_json(&self.scope, route(id), params, NO_QUERY, json(body)?)
            .await
    }

    async fn post_empty<B: Serialize + ?Sized>(&self, id: &str, params: &[&str], body: &B) -> Result<(), ClientError> {
        self.client
            .exec(&self.scope, route(id), params, NO_QUERY, json(body)?, None)
            .await
            .map(drop)
    }

    /// Take the lease (only hermit's current sandbox of the conversation gets it).
    pub async fn lease(&self) -> Result<LeaseResponse, ClientError> {
        self.post("lease", &[], &Empty {}).await
    }

    /// The conversation's `~/.claude` tree.
    pub async fn cc_tree(&self) -> Result<CcTree, ClientError> {
        self.client
            .exec_json(&self.scope, route("cc_tree"), &[], NO_QUERY, Body::None)
            .await
    }

    /// A file's bytes (streams; 206 with a range).
    pub async fn cc_file(&self, q: &CcFileQuery) -> Result<reqwest::Response, ClientError> {
        self.client
            .exec(&self.scope, route("cc_file"), &[], Some(q), Body::None, None)
            .await
    }

    /// Write bytes (<= 16 MiB).
    pub async fn cc_write(
        &self,
        q: &CcWriteQuery,
        bytes: impl Into<reqwest::Body>,
    ) -> Result<CcSizeVersion, ClientError> {
        let body = Body::Raw(bytes.into(), "application/octet-stream".to_owned());
        self.client
            .exec_json(&self.scope, route("cc_write"), &[], Some(q), body)
            .await
    }

    /// Truncate.
    pub async fn cc_truncate(&self, req: &CcTruncate) -> Result<CcSizeVersion, ClientError> {
        self.post("cc_truncate", &[], req).await
    }

    /// Rename (replaces `to`).
    pub async fn cc_rename(&self, req: &CcRename) -> Result<CcVersion, ClientError> {
        self.post("cc_rename", &[], req).await
    }

    /// Unlink.
    pub async fn cc_unlink(&self, req: &CcPathOp) -> Result<(), ClientError> {
        self.post_empty("cc_unlink", &[], req).await
    }

    /// Make a directory.
    pub async fn cc_mkdir(&self, req: &CcMkdir) -> Result<(), ClientError> {
        self.post_empty("cc_mkdir", &[], req).await
    }

    /// Remove an empty directory.
    pub async fn cc_rmdir(&self, req: &CcPathOp) -> Result<(), ClientError> {
        self.post_empty("cc_rmdir", &[], req).await
    }

    /// Mode and mtime.
    pub async fn cc_setattr(&self, req: &CcSetattr) -> Result<CcVersion, ClientError> {
        self.post("cc_setattr", &[], req).await
    }

    /// The command long-poll.
    pub async fn commands(&self, q: &CommandsQuery) -> Result<CommandsResponse, ClientError> {
        self.client
            .exec_json(&self.scope, route("commands"), &[], Some(q), Body::None)
            .await
    }

    /// Ack a command.
    pub async fn ack_command(&self, cmd_seq: u64, req: &AckRequest) -> Result<(), ClientError> {
        self.post_empty("ack_command", &[&cmd_seq.to_string()], req).await
    }

    /// Open a turn Claude Code started by itself.
    pub async fn agent_turn(&self, req: &AgentTurnRequest) -> Result<Admitted<()>, ClientError> {
        let resp = self
            .client
            .exec(&self.scope, route("agent_turn"), &[], NO_QUERY, json(req)?, None)
            .await?;
        Ok(if resp.status().as_u16() == 200 {
            Admitted::Repeat(())
        } else {
            Admitted::New(())
        })
    }

    /// Report a turn's state.
    pub async fn turn_state(&self, turn_id: Uuid, req: &TurnStateReport) -> Result<(), ClientError> {
        self.post_empty("turn_state", &[&turn_id.to_string()], req).await
    }

    /// Send live frames.
    pub async fn live_frames(&self, turn_id: Uuid, req: &LiveFramesRequest) -> Result<(), ClientError> {
        self.post_empty("live_frames", &[&turn_id.to_string()], req).await
    }

    /// Report task events.
    pub async fn tasks(&self, req: &TasksReport) -> Result<(), ClientError> {
        self.post_empty("tasks", &[], req).await
    }

    /// Open an ask.
    pub async fn ask_open(&self, req: &AskReport) -> Result<(), ClientError> {
        self.post_empty("ask_open", &[], req).await
    }

    /// An ask ended.
    pub async fn ask_resolved(&self, ask_id: &str, req: &AskResolvedReport) -> Result<(), ClientError> {
        self.post_empty("ask_resolved", &[ask_id], req).await
    }

    /// The heartbeat.
    pub async fn heartbeat(&self, req: &Heartbeat) -> Result<(), ClientError> {
        self.post_empty("heartbeat", &[], req).await
    }

    /// A blob the conversation references (streams).
    pub async fn sandbox_blob_get(&self, sha: &str, q: &SandboxBlobQuery) -> Result<reqwest::Response, ClientError> {
        self.client
            .exec(
                &self.scope,
                route("sandbox_blob_get"),
                &[sha],
                Some(q),
                Body::None,
                None,
            )
            .await
    }

    /// Store a blob (streamed, verified against `sha`).
    pub async fn sandbox_blob_put(
        &self,
        sha: &str,
        bytes: impl Into<reqwest::Body>,
    ) -> Result<Admitted<()>, ClientError> {
        let body = Body::Raw(bytes.into(), "application/octet-stream".to_owned());
        let resp = self
            .client
            .exec(&self.scope, route("sandbox_blob_put"), &[sha], NO_QUERY, body, None)
            .await?;
        Ok(if resp.status().as_u16() == 200 {
            Admitted::Repeat(())
        } else {
            Admitted::New(())
        })
    }

    /// Stage a send of files.
    pub async fn sent_files(&self, req: &SentFilesRequest) -> Result<SentFilesResponse, ClientError> {
        self.post("sent_files", &[], req).await
    }

    /// Commit a staged send.
    pub async fn sent_files_commit(&self, tool_use_id: &str) -> Result<SentFilesCommitted, ClientError> {
        self.post("sent_files_commit", &[tool_use_id], &Empty {}).await
    }

    /// The person's schedules (every conversation).
    pub async fn list_schedules(&self) -> Result<SchedulesList, ClientError> {
        self.client
            .exec_json(&self.scope, route("list_schedules"), &[], NO_QUERY, Body::None)
            .await
    }

    /// Arm a schedule in the sandbox's conversation.
    pub async fn create_schedule(&self, req: &CreateSchedule) -> Result<Schedule, ClientError> {
        self.post("create_schedule", &[], req).await
    }

    /// One schedule with its last runs.
    pub async fn get_schedule(&self, schedule_id: &str) -> Result<Schedule, ClientError> {
        self.client
            .exec_json(&self.scope, route("get_schedule"), &[schedule_id], NO_QUERY, Body::None)
            .await
    }

    /// Change a schedule.
    pub async fn update_schedule(&self, schedule_id: &str, req: &UpdateSchedule) -> Result<Schedule, ClientError> {
        self.post("update_schedule", &[schedule_id], req).await
    }

    /// Remove a schedule.
    pub async fn delete_schedule(&self, schedule_id: &str) -> Result<(), ClientError> {
        self.client
            .exec(
                &self.scope,
                route("delete_schedule"),
                &[schedule_id],
                NO_QUERY,
                Body::None,
                None,
            )
            .await
            .map(drop)
    }
}

/// hermit's, the ops tools' and the admin tooling's calls (16.3, 16.4). Made by [`Client::service`].
#[derive(Debug, Clone)]
pub struct ServiceCalls<'a> {
    client: &'a Client,
    scope: Scope,
}

#[allow(clippy::missing_errors_doc)] // every method: ClientError
impl ServiceCalls<'_> {
    forward_request_id!();

    async fn get<Q: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        id: &str,
        params: &[&str],
        q: Option<&Q>,
    ) -> Result<T, ClientError> {
        self.client
            .exec_json(&self.scope, route(id), params, q, Body::None)
            .await
    }

    /// A conversation the deployment starts for a person (caller hermit).
    pub async fn hermit_create_conversation(
        &self,
        req: &HermitCreateRequest,
    ) -> Result<Admitted<CreateResponse>, ClientError> {
        self.client
            .exec_admitted(
                &self.scope,
                route("hermit_create_conversation"),
                &[],
                NO_QUERY,
                json(req)?,
            )
            .await
    }

    /// A conversation as the deployment reads it: any person's, deleted ones too (caller hermit).
    pub async fn hermit_conversation(&self, id: Uuid) -> Result<Conversation, ClientError> {
        self.get("hermit_conversation", &[&id.to_string()], NO_QUERY).await
    }

    /// Closes the conversation to its person's messages (`reason`), or opens it again (`None`) (caller hermit).
    pub async fn hermit_close_conversation(&self, id: Uuid, reason: Option<&str>) -> Result<Conversation, ClientError> {
        let req = crate::ops::HermitCloseRequest {
            reason: reason.map(str::to_owned),
        };
        self.client
            .exec_json(
                &self.scope,
                route("hermit_close_conversation"),
                &[&id.to_string()],
                NO_QUERY,
                json(&req)?,
            )
            .await
    }

    /// Ends the conversation: into Recently deleted, as its person's delete does (caller hermit).
    pub async fn hermit_delete_conversation(&self, id: Uuid) -> Result<Conversation, ClientError> {
        self.client
            .exec_json(
                &self.scope,
                route("hermit_delete_conversation"),
                &[&id.to_string()],
                NO_QUERY,
                json(&Empty {})?,
            )
            .await
    }

    /// A turn the deployment sends into a conversation as its person (caller hermit).
    pub async fn hermit_send_turn(
        &self,
        id: Uuid,
        req: &HermitSendRequest,
    ) -> Result<Admitted<SendResponse>, ClientError> {
        self.client
            .exec_admitted(
                &self.scope,
                route("hermit_send_turn"),
                &[&id.to_string()],
                NO_QUERY,
                json(req)?,
            )
            .await
    }

    /// Purge every conversation of a person hermit no longer allows (caller hermit).
    pub async fn person_purge(
        &self,
        person: &str,
        req: &PersonPurgeRequest,
    ) -> Result<PersonPurgeResponse, ClientError> {
        self.client
            .exec_json(&self.scope, route("person_purge"), &[person], NO_QUERY, json(req)?)
            .await
    }

    /// The workspace ids magpie still references (caller hermit).
    pub async fn workspaces_exists(
        &self,
        req: &WorkspacesExistsRequest,
    ) -> Result<WorkspacesExistsResponse, ClientError> {
        self.client
            .exec_json(&self.scope, route("workspaces_exists"), &[], NO_QUERY, json(req)?)
            .await
    }

    /// Whether the switch's import is complete (caller hermit's `legacy-cleanup`).
    pub async fn legacy_import(&self) -> Result<LegacyImportStatus, ClientError> {
        self.get("legacy_import", &[], NO_QUERY).await
    }

    /// Per-person counts.
    pub async fn ops_people(&self, q: &PeopleQuery) -> Result<PeopleResponse, ClientError> {
        self.get("ops_people", &[], Some(q)).await
    }

    /// The view-as log.
    pub async fn ops_view_as(&self, q: &ViewAsLogQuery) -> Result<ViewAsLog, ClientError> {
        self.get("ops_view_as", &[], Some(q)).await
    }

    /// Store stats.
    pub async fn ops_stats(&self) -> Result<OpsStats, ClientError> {
        self.get("ops_stats", &[], NO_QUERY).await
    }

    /// Admin: conversations of a person.
    pub async fn admin_conversations(&self, q: &AdminListQuery) -> Result<AdminConversationList, ClientError> {
        self.get("admin_conversations", &[], Some(q)).await
    }

    /// Admin: one conversation with its operational fields.
    pub async fn admin_conversation(&self, id: Uuid) -> Result<AdminConversation, ClientError> {
        self.get("admin_conversation", &[&id.to_string()], NO_QUERY).await
    }

    /// Admin: the events page.
    pub async fn admin_events(&self, id: Uuid, q: &EventsQuery) -> Result<EventsPage, ClientError> {
        self.get("admin_events", &[&id.to_string()], Some(q)).await
    }

    /// Admin: the timeline page.
    pub async fn admin_timeline(&self, id: Uuid, q: &TimelineQuery) -> Result<TimelinePage, ClientError> {
        self.get("admin_timeline", &[&id.to_string()], Some(q)).await
    }

    /// Admin: the raw view's transcript lines of any conversation, with its owner and title (audited).
    pub async fn admin_transcript(&self, id: Uuid, q: &TranscriptQuery) -> Result<TranscriptPage, ClientError> {
        self.get("admin_transcript", &[&id.to_string()], Some(q)).await
    }

    /// Admin: everyone's schedules with their last runs, and the pause-all switch.
    pub async fn admin_schedules(&self) -> Result<AdminSchedules, ClientError> {
        self.get("admin_schedules", &[], NO_QUERY).await
    }

    /// Admin: set the pause-all switch.
    pub async fn admin_schedules_pause(&self, req: &PauseAll) -> Result<AdminSchedules, ClientError> {
        self.client
            .exec_json(&self.scope, route("admin_schedules_pause"), &[], NO_QUERY, json(req)?)
            .await
    }
}
