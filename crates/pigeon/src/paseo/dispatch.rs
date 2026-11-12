//! Every client message and what pigeon does with it (spec 7.3, 7.5, 6.4).
//!
//! Messages without an upstream are answered in the reader (7.5.1); the rest run as their own tasks, concurrently,
//! each answering through the socket's ordered queue and always answering a request that has a `requestId` (an
//! unexpected failure is `rpc_error failed`). Conversation-scoped requests of a conversation that [`Hub::delegate`]
//! hands elsewhere go to that delegate (WP-50's per-conversation owner switch); everything else is magpie's.

use std::collections::HashSet;
use std::sync::Arc;

use base64::Engine as _;
use magpie_api::client::{ClientError, PersonCalls};
use magpie_api::conversations::{Conversation, CreateRequest, ListQuery, ListState, RenameRequest};
use magpie_api::errors::{ErrorCode, Refusal};
use magpie_api::rows::{TIMELINE_DEFAULT, TIMELINE_MAX, TimelineQuery};
use magpie_api::turns::{AgentPermissionResponse, FileRef, ImageRef, SendRequest};
use magpie_api::uploads::UploadKind;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::ProjectCtx;
use super::mapping::{
    View, WORKSPACE_PREFIX, agent_snapshot, project_descriptor, project_placement, workspace_descriptor,
};
use super::messages::{self, has_sync, now_iso, rpc_error, session, str_field, sync_snapshot};
use super::pushes;
use super::strings::{self, Locale};
use super::transport::{Conn, ListKind, MAX_TIMELINE_AGENTS};
use crate::App;
use crate::feeds::Hub;

/// A failed magpie call, as the handlers tell it apart.
#[derive(Debug, Clone)]
pub enum Fail {
    /// 404: not the person's, or gone.
    NotFound,
    /// A 4xx with its code.
    Refused(Refusal),
    /// No answer, a timeout, a 5xx.
    Unreachable,
}

/// Classifies a magpie error.
#[must_use]
pub fn fail(e: ClientError) -> Fail {
    match e {
        ClientError::Api(r) if r.status == 404 => Fail::NotFound,
        // a refusal to act that names its reason (503 view_as_not_recorded), not an outage
        ClientError::Api(r) if r.code == ErrorCode::ViewAsNotRecorded => Fail::Refused(r),
        ClientError::Api(r) if r.status < 500 => Fail::Refused(r),
        _ => Fail::Unreachable,
    }
}

impl Fail {
    fn code(&self) -> Option<&ErrorCode> {
        match self {
            Fail::Refused(r) => Some(&r.code),
            _ => None,
        }
    }
}

/// The product name, the locale and the default model id: what the object projections need (7.7).
#[must_use]
pub fn view_parts(app: &App) -> (String, Locale, Option<String>) {
    let ids = app.identities();
    let model =
        crate::http::models_cached(app).and_then(|m| m.get("DEFAULT").and_then(Value::as_str).map(str::to_owned));
    (ids.product_name(), Locale::from_zh(ids.is_zh()), model)
}

/// The kinds that name one conversation (by `agentId` or `workspaceId`), which a delegate may own.
fn conversation_of(kind: &str, m: &Value) -> Option<Uuid> {
    let id = match kind {
        "fetch_agent_request"
        | "fetch_agent_timeline_request"
        | "agent.timeline.list_prompts.request"
        | "send_agent_message_request"
        | "cancel_agent_request"
        | "update_agent_request"
        | "archive_agent_request"
        | "delete_agent_request"
        | "refresh_agent_request"
        | "agent_permission_response" => str_field(m, "agentId")?,
        "workspace.title.set.request" | "archive_workspace_request" => {
            str_field(m, "workspaceId")?.strip_prefix(WORKSPACE_PREFIX)?
        }
        _ => return None,
    };
    Uuid::parse_str(id).ok()
}

/// The request types handled by a task with an upstream call.
fn is_upstream(kind: &str) -> bool {
    matches!(
        kind,
        "fetch_workspaces_request"
            | "fetch_agents_request"
            | "fetch_agent_request"
            | "fetch_agent_history_request"
            | "fetch_agent_timeline_request"
            | "agent.timeline.list_prompts.request"
            | "send_agent_message_request"
            | "workspace.create.request"
            | "creation.subscribe.request"
            | "cancel_agent_request"
            | "update_agent_request"
            | "workspace.title.set.request"
            | "archive_workspace_request"
            | "archive_agent_request"
            | "delete_agent_request"
            | "refresh_agent_request"
            | "agent_permission_response"
            | "get_providers_snapshot_request"
            | "project.list.request"
    )
}

/// Refused on a view-as socket without an upstream call (6.4); `refresh_agent_request` only when it restores, which
/// the handler decides.
fn view_as_refused(kind: &str) -> bool {
    matches!(
        kind,
        "update_agent_request"
            | "workspace.title.set.request"
            | "archive_workspace_request"
            | "archive_agent_request"
            | "delete_agent_request"
            | "agent_permission_response"
    )
}

/// Dispatches one session message (7.3).
pub fn message(app: &App, hub: &Hub, conn: &Arc<Conn>, m: Value, hold: crate::paseo::transport::Hold) {
    let kind = str_field(&m, "type").unwrap_or("").to_owned();
    let rid = str_field(&m, "requestId").unwrap_or("").to_owned();
    match kind.as_str() {
        "client_heartbeat" | "abort_request" | "audio_played" | "register_push_token" => return,
        "subscription.release.request" => {
            let sub = str_field(&m, "subscriptionId").unwrap_or("").to_owned();
            conn.release(hub, conn, &sub);
            conn.reply(
                "subscription.release.response",
                &json!({"requestId": rid, "subscriptionId": sub}),
            );
            return;
        }
        "agent.timeline.set_subscription.request" => {
            set_timeline(hub, conn, &m, &rid);
            return;
        }
        _ => {}
    }
    let product = app.identities().product_name();
    if let Some((reply, payload)) = messages::static_reply(&kind, &m, &rid, &product, "") {
        conn.reply(reply, &payload);
        return;
    }
    if !is_upstream(&kind) {
        if !rid.is_empty() {
            conn.reply(
                "rpc_error",
                &rpc_error(&rid, &kind, strings::NOT_AVAILABLE, "unsupported"),
            );
        }
        return;
    }
    if conn.viewer.is_some() && view_as_refused(&kind) {
        conn.reply(
            "rpc_error",
            &rpc_error(&rid, &kind, strings::VIEW_AS_READ_ONLY, "view_as_read_only"),
        );
        return;
    }
    let guard = conn.in_flight();
    let h = Handler {
        app: app.clone(),
        hub: hub.clone(),
        conn: conn.clone(),
        kind,
        rid,
        m,
    };
    tokio::spawn(async move {
        let _guard = guard;
        let _hold = hold;
        let (conn, kind, rid) = (h.conn.clone(), h.kind.clone(), h.rid.clone());
        let started = std::time::Instant::now();
        let app = h.app.clone();
        let task = tokio::spawn(async move { h.run().await });
        if task.await.is_err() && !rid.is_empty() {
            let locale = Locale::from_zh(app.identities().is_zh());
            conn.reply("rpc_error", &rpc_error(&rid, &kind, locale.unreachable(), "failed"));
        }
        app.0.metrics.rpc(&kind, started.elapsed().as_secs_f64());
    });
}

fn set_timeline(hub: &Hub, conn: &Arc<Conn>, m: &Value, rid: &str) {
    let mut ids: Vec<String> = Vec::new();
    let mut set = HashSet::new();
    for v in m.get("agentIds").and_then(Value::as_array).into_iter().flatten() {
        let Some(s) = v.as_str() else { continue };
        let Ok(id) = Uuid::parse_str(s) else { continue };
        if set.len() >= MAX_TIMELINE_AGENTS {
            break;
        }
        if set.insert(id) {
            ids.push(s.to_owned());
        }
    }
    let sub = conn.renew_timeline_sub();
    hub.set_timeline(conn, &set);
    conn.reply(
        "agent.timeline.set_subscription.response",
        &json!({"requestId": rid, "agentIds": ids, "subscriptionId": sub}),
    );
}

struct Handler {
    app: App,
    hub: Hub,
    conn: Arc<Conn>,
    kind: String,
    rid: String,
    m: Value,
}

struct Owned {
    product: String,
    locale: Locale,
    model: Option<String>,
}

impl Owned {
    fn view(&self) -> View<'_> {
        View {
            product: &self.product,
            locale: self.locale,
            model: self.model.as_deref(),
        }
    }
}

impl Handler {
    fn reply(&self, kind: &str, payload: &Value) {
        self.conn.reply(kind, payload);
    }

    fn calls(&self) -> PersonCalls<'_> {
        self.conn.calls(&self.app.0.magpie)
    }

    fn turn_calls(&self) -> PersonCalls<'_> {
        self.conn.calls(&self.app.0.magpie_turns)
    }

    fn owned(&self) -> Owned {
        let (product, locale, model) = view_parts(&self.app);
        Owned { product, locale, model }
    }

    fn locale(&self) -> Locale {
        Locale::from_zh(self.app.identities().is_zh())
    }

    fn rpc_failed(&self, error: &str) {
        self.reply("rpc_error", &rpc_error(&self.rid, &self.kind, error, "failed"));
    }

    fn agent_id(&self) -> Option<Uuid> {
        str_field(&self.m, "agentId").and_then(|s| Uuid::parse_str(s).ok())
    }

    async fn run(self) {
        if let Some(c) = conversation_of(&self.kind, &self.m)
            && let Some(d) = self.hub.delegate(&self.conn.person, &c)
        {
            d.request(self.conn.clone(), self.kind.clone(), self.m.clone()).await;
            return;
        }
        match self.kind.as_str() {
            "fetch_workspaces_request" => self.fetch_list(ListKind::Workspaces).await,
            "fetch_agents_request" => self.fetch_list(ListKind::Agents).await,
            "fetch_agent_request" => self.fetch_agent().await,
            "fetch_agent_history_request" => self.fetch_history().await,
            "fetch_agent_timeline_request" => self.fetch_timeline().await,
            "agent.timeline.list_prompts.request" => self.list_prompts().await,
            "send_agent_message_request" => self.send().await,
            "workspace.create.request" => self.create().await,
            "creation.subscribe.request" => self.creation().await,
            "cancel_agent_request" => self.cancel().await,
            "update_agent_request" => self.update_agent().await,
            "workspace.title.set.request" => self.title_set().await,
            "archive_workspace_request" | "archive_agent_request" => self.archive().await,
            "delete_agent_request" => self.delete().await,
            "refresh_agent_request" => self.refresh().await,
            "agent_permission_response" => self.answer().await,
            "get_providers_snapshot_request" => self.providers().await,
            "project.list.request" => self.project_list().await,
            _ => {}
        }
    }

    fn page(&self, default: u32) -> (u32, Option<String>) {
        let page = self.m.get("page");
        let limit = page
            .and_then(|p| p.get("limit"))
            .and_then(Value::as_u64)
            .map_or(default, |l| u32::try_from(l.clamp(1, 200)).unwrap_or(200));
        let cursor = page
            .and_then(|p| str_field(p, "cursor"))
            .filter(|c| !c.is_empty())
            .map(str::to_owned);
        (limit, cursor)
    }

    /// 7.5.2: both list requests, one magpie call for the pair.
    async fn fetch_list(&self, kind: ListKind) {
        let (limit, cursor) = self.page(magpie_api::conversations::LIST_MAX);
        let first = cursor.is_none();
        if first && messages::subscribe(&self.m).is_some() {
            // a subscription made again: no live change goes out until the new snapshot's join sets the floor, so
            // no change older than the snapshot follows it (8.3)
            self.conn.clear_floor(kind);
        }
        let Ok(list) = self.hub.list(&self.app, &self.conn, cursor, limit).await else {
            return self.rpc_failed(self.locale().unreachable());
        };
        let o = self.owned();
        let view = o.view();
        let sub = messages::subscribe(&self.m).map(|id| self.conn.subscribe_list(kind, id));
        let page_info = json!({"nextCursor": list.next_cursor, "prevCursor": null,
                               "hasMore": list.next_cursor.is_some()});
        let sync = sync_snapshot(&list.list.generation, list.list.head_seq, has_sync(&self.m));
        match kind {
            ListKind::Workspaces => {
                let entries: Vec<Value> = list
                    .conversations
                    .iter()
                    .map(|c| workspace_descriptor(c, &view))
                    .collect();
                let empty = if first && list.conversations.is_empty() {
                    vec![project_descriptor(&view)]
                } else {
                    vec![]
                };
                self.reply(
                    "fetch_workspaces_response",
                    &json!({"requestId": self.rid, "subscriptionId": sub, "entries": entries, "emptyProjects": empty,
                            "pageInfo": page_info, "sync": sync}),
                );
            }
            ListKind::Agents => {
                let entries: Vec<Value> = list
                    .conversations
                    .iter()
                    .map(|c| json!({"agent": agent_snapshot(c, &view), "project": project_placement(c, &view)}))
                    .collect();
                self.reply(
                    "fetch_agents_response",
                    &json!({"requestId": self.rid, "subscriptionId": sub, "entries": entries, "pageInfo": page_info,
                            "sync": sync}),
                );
            }
        }
        if sub.is_some() && first {
            self.hub.join_list(&self.conn, kind, list.list.head_seq);
        }
        if kind == ListKind::Agents {
            // a tab that connects while a sandbox is being made shows the chip (gap-3 2.4)
            for c in list
                .conversations
                .iter()
                .filter(|c| c.sandbox == Some(magpie_api::conversations::SandboxPhase::Preparing))
            {
                self.reply(
                    messages::sandbox_status_type(),
                    &pushes::sandbox_status(&c.id, "preparing"),
                );
            }
        }
    }

    async fn conversation(&self, id: Uuid) -> Result<Conversation, Fail> {
        self.calls().get_conversation(id).await.map_err(fail)
    }

    async fn fetch_agent(&self) {
        let o = self.owned();
        let found = match self.agent_id() {
            None => Err(Fail::NotFound),
            Some(id) => self.conversation(id).await,
        };
        match found {
            Ok(c) => self.reply(
                "fetch_agent_response",
                &json!({"requestId": self.rid, "agent": agent_snapshot(&c, &o.view()),
                        "project": project_placement(&c, &o.view()), "error": null}),
            ),
            Err(Fail::NotFound) => self.reply(
                "fetch_agent_response",
                &json!({"requestId": self.rid, "agent": null, "project": null, "error": strings::NOT_FOUND}),
            ),
            Err(_) => self.rpc_failed(o.locale.unreachable()),
        }
    }

    async fn fetch_history(&self) {
        let (limit, cursor) = self.page(50);
        let search = str_field(&self.m, "search")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let q = ListQuery {
            state: Some(ListState::Deleted),
            limit: Some(limit),
            cursor,
            search,
        };
        let o = self.owned();
        match self.calls().list_conversations(&q).await {
            Ok(list) => {
                let view = o.view();
                let entries: Vec<Value> = list
                    .conversations
                    .iter()
                    .map(|c| json!({"agent": agent_snapshot(c, &view), "project": project_placement(c, &view)}))
                    .collect();
                self.reply(
                    "fetch_agent_history_response",
                    &json!({"requestId": self.rid, "entries": entries,
                            "pageInfo": {"nextCursor": list.next_cursor, "prevCursor": null,
                                         "hasMore": list.next_cursor.is_some()}}),
                );
            }
            Err(_) => self.rpc_failed(o.locale.unreachable()),
        }
    }

    fn not_found_timeline(&self, direction: &str) -> Value {
        json!({"requestId": self.rid, "agentId": str_field(&self.m, "agentId").unwrap_or(""), "agent": null,
               "direction": direction, "projection": "projected", "epoch": "", "reset": false, "staleCursor": false,
               "gap": false, "window": {"minSeq": 0, "maxSeq": 0, "nextSeq": 0}, "startCursor": null,
               "endCursor": null, "hasOlder": false, "hasNewer": false, "entries": [],
               "error": strings::NOT_FOUND})
    }

    /// 7.5.3.
    async fn fetch_timeline(&self) {
        let direction = match str_field(&self.m, "direction") {
            Some("before") => "before",
            Some("after") => "after",
            _ => "tail",
        };
        let Some(id) = self.agent_id() else {
            return self.reply("fetch_agent_timeline_response", &self.not_found_timeline(direction));
        };
        let limit = match self.m.get("limit").and_then(Value::as_u64) {
            None => TIMELINE_DEFAULT,
            Some(0) => TIMELINE_MAX,
            Some(l) => u32::try_from(l.min(u64::from(TIMELINE_MAX))).unwrap_or(TIMELINE_MAX),
        };
        let cursor = self.m.get("cursor").filter(|c| c.is_object());
        let cursor_epoch = cursor.and_then(|c| str_field(c, "epoch")).map(str::to_owned);
        let cursor_seq = cursor.and_then(|c| c.get("seq")).and_then(Value::as_u64);
        let tail = TimelineQuery {
            tail: true,
            limit: Some(limit),
            ..TimelineQuery::default()
        };
        let q = match (direction, cursor_seq) {
            ("before", Some(s)) => TimelineQuery {
                before: Some(s),
                limit: Some(limit),
                ..TimelineQuery::default()
            },
            ("after", Some(s)) => TimelineQuery {
                after: Some(s),
                limit: Some(limit),
                ..TimelineQuery::default()
            },
            _ => tail.clone(),
        };
        let calls = self.calls();
        let (page, conv) = tokio::join!(calls.timeline(id, &q), self.conversation(id));
        let o = self.owned();
        let mut page = match (page.map_err(fail), conv.as_ref()) {
            (Err(Fail::NotFound), _) | (_, Err(Fail::NotFound)) => {
                return self.reply("fetch_agent_timeline_response", &self.not_found_timeline(direction));
            }
            (Ok(p), Ok(_)) => p,
            _ => return self.rpc_failed(o.locale.unreachable()),
        };
        let mut stale = false;
        if let Some(e) = &cursor_epoch
            && *e != page.epoch
        {
            stale = true;
            page = match calls.timeline(id, &tail).await {
                Ok(p) => p,
                Err(e) => {
                    if matches!(fail(e), Fail::NotFound) {
                        self.reply("fetch_agent_timeline_response", &self.not_found_timeline(direction));
                    } else {
                        self.rpc_failed(o.locale.unreachable());
                    }
                    return;
                }
            };
        }
        let Ok(conv) = conv else { return };
        let signer = self.app.blob_signer();
        let ctx = ProjectCtx {
            zh: o.locale == Locale::Zh,
            signer: &signer,
        };
        let entries: Vec<Value> = page
            .rows
            .iter()
            .filter_map(|r| self.app.0.projector.project(r, &ctx))
            .collect();
        let epoch = page.epoch.clone();
        let start = page.rows.first().map(|r| json!({"epoch": epoch, "seq": r.seq_start}));
        let end = page.rows.last().map(|r| json!({"epoch": epoch, "seq": r.seq_end}));
        let mut payload = json!({
            "requestId": self.rid, "agentId": id, "agent": agent_snapshot(&conv, &o.view()), "direction": direction,
            "projection": "projected", "epoch": epoch, "reset": stale, "staleCursor": stale, "gap": false,
            "window": {"minSeq": page.min_seq, "maxSeq": page.max_seq, "nextSeq": page.max_seq + 1},
            "startCursor": start, "endCursor": end, "hasOlder": page.has_older, "hasNewer": page.has_newer,
            "entries": entries, "error": null,
        });
        if let (Some(mw), Some(p)) = (
            self.m.get("mergeWindow").filter(|m| m.is_boolean()),
            payload.as_object_mut(),
        ) {
            p.insert("mergeWindow".into(), mw.clone());
        }
        self.reply("fetch_agent_timeline_response", &payload);
    }

    async fn list_prompts(&self) {
        let agent = str_field(&self.m, "agentId").unwrap_or("").to_owned();
        let not_found = || json!({"requestId": self.rid, "agentId": agent, "epoch": "", "prompts": [], "error": strings::NOT_FOUND});
        let Some(id) = self.agent_id() else {
            return self.reply("agent.timeline.list_prompts.response", &not_found());
        };
        match self.calls().prompts(id).await.map_err(fail) {
            Ok(p) => {
                let prompts: Vec<Value> = p
                    .prompts
                    .iter()
                    .map(|x| {
                        let preview: String = x.preview.chars().take(200).collect();
                        json!({"seq": x.seq, "timestamp": x.timestamp, "preview": preview})
                    })
                    .collect();
                self.reply(
                    "agent.timeline.list_prompts.response",
                    &json!({"requestId": self.rid, "agentId": agent, "epoch": p.epoch, "prompts": prompts,
                            "error": null}),
                );
            }
            Err(Fail::NotFound) => self.reply("agent.timeline.list_prompts.response", &not_found()),
            Err(_) => self.rpc_failed(self.locale().unreachable()),
        }
    }

    /// Uploads a send's images (7.5.4 step 1); the error sentence when one fails.
    async fn upload_images(&self, images: Option<&Value>) -> Result<Vec<ImageRef>, String> {
        let locale = self.locale();
        let mut out = Vec::new();
        for (i, img) in images.and_then(Value::as_array).into_iter().flatten().enumerate() {
            let data = str_field(img, "data").unwrap_or("");
            let mime = str_field(img, "mimeType")
                .unwrap_or("application/octet-stream")
                .to_owned();
            let data = data.split_once(";base64,").map_or(data, |(_, d)| d).to_owned();
            let body = reqwest::Body::wrap_stream(decode_slices(data));
            let name = format!("image-{}.{}", i + 1, mime.rsplit('/').next().unwrap_or("bin"));
            match self
                .conn
                .calls(&self.app.0.magpie_upload)
                .upload(&name, UploadKind::Image, &mime, body)
                .await
            {
                Ok(u) => out.push(ImageRef { upload_id: u.upload_id }),
                Err(e) => {
                    return Err(match fail(e).code() {
                        Some(ErrorCode::TooLarge) => locale.too_large().to_owned(),
                        Some(ErrorCode::UnsupportedType) => locale.unsupported_type().to_owned(),
                        _ => locale.upload_failed().to_owned(),
                    });
                }
            }
        }
        Ok(out)
    }

    fn files(attachments: Option<&Value>) -> Vec<FileRef> {
        attachments
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|a| {
                let path = str_field(a, "path").or_else(|| str_field(a, "id"))?.to_owned();
                Some(FileRef {
                    name: str_field(a, "fileName")
                        .map_or_else(|| path.rsplit('/').next().unwrap_or("").to_owned(), str::to_owned),
                    mime_type: str_field(a, "mimeType").unwrap_or("").to_owned(),
                    size: a.get("size").and_then(Value::as_u64).unwrap_or(0),
                    path,
                })
            })
            .collect()
    }

    /// The sentence for a refused send or create (7.5.4's table).
    fn send_error(&self, f: &Fail) -> String {
        let l = self.locale();
        match f {
            Fail::NotFound => strings::NOT_FOUND.to_owned(),
            Fail::Refused(r) => match r.code {
                ErrorCode::TurnRunning => l.turn_running().to_owned(),
                ErrorCode::ConversationDeleted => l.conversation_deleted().to_owned(),
                ErrorCode::ViewAsNotRecorded => l.view_as_not_recorded().to_owned(),
                ErrorCode::TooLarge => l.too_large().to_owned(),
                ErrorCode::UnsupportedType => l.unsupported_type().to_owned(),
                _ => l.unreachable().to_owned(),
            },
            Fail::Unreachable => l.unreachable().to_owned(),
        }
    }

    async fn send(&self) {
        let agent = str_field(&self.m, "agentId").unwrap_or("").to_owned();
        let answer = |accepted: bool, error: Option<String>| json!({"requestId": self.rid, "agentId": agent, "accepted": accepted, "error": error});
        let Some(id) = self.agent_id() else {
            return self.reply(
                "send_agent_message_response",
                &answer(false, Some(strings::NOT_FOUND.into())),
            );
        };
        let images = match self.upload_images(self.m.get("images")).await {
            Ok(i) => i,
            Err(e) => return self.reply("send_agent_message_response", &answer(false, Some(e))),
        };
        let req = SendRequest {
            client_message_id: str_field(&self.m, "messageId")
                .filter(|s| !s.is_empty())
                .map_or_else(|| Uuid::new_v4().to_string(), str::to_owned),
            text: str_field(&self.m, "text").unwrap_or("").to_owned(),
            images,
            files: Self::files(self.m.get("attachments")),
        };
        match self.turn_calls().send_turn(id, &req).await {
            Ok(_) => self.reply("send_agent_message_response", &answer(true, None)),
            Err(e) => {
                let f = fail(e);
                self.reply("send_agent_message_response", &answer(false, Some(self.send_error(&f))));
            }
        }
    }

    /// 7.5.5.
    #[allow(clippy::too_many_lines)] // the create's steps and both answers, in order
    async fn create(&self) {
        let key = str_field(&self.m, "idempotencyKey")
            .filter(|k| !k.is_empty())
            .map_or_else(|| Uuid::new_v4().to_string(), str::to_owned);
        let agent = self.m.get("agent").filter(|a| a.is_object());
        let first = self.m.get("firstAgentContext").filter(|a| a.is_object());
        let text = agent
            .and_then(|a| str_field(a, "initialPrompt"))
            .or_else(|| first.and_then(|f| str_field(f, "prompt")))
            .unwrap_or("")
            .to_owned();
        let client_message_id = agent
            .and_then(|a| str_field(a, "clientMessageId"))
            .map_or_else(|| format!("{key}:initial-message"), str::to_owned);
        let pick = |k: &str| agent.and_then(|a| a.get(k)).or_else(|| first.and_then(|f| f.get(k)));
        let subscribe = messages::subscribe(&self.m).is_some();
        let locale = self.locale();
        let failed = |error: String, code: Option<String>| {
            if subscribe {
                let mut u = Map::new();
                u.insert("kind".into(), json!("workspace"));
                u.insert("idempotencyKey".into(), json!(key));
                u.insert("revision".into(), json!(0));
                u.insert("phase".into(), json!("failed"));
                u.insert("workspaceId".into(), Value::Null);
                u.insert("agentId".into(), Value::Null);
                u.insert("error".into(), json!(error));
                if let Some(c) = code {
                    u.insert("errorCode".into(), json!(c));
                }
                self.reply("workspace.create.update", &Value::Object(u));
            }
            self.reply(
                "workspace.create.response",
                // `agent` is optional and not nullable in the app's schema: left out
                &json!({"requestId": self.rid, "workspace": null, "setupTerminalId": null, "error": error}),
            );
        };
        // the deployment starts every conversation itself (hermit's `conversations.peopleCreate` false)
        if !self.app.identities().people_create() {
            return failed(
                locale.create_closed().to_owned(),
                Some("conversations_closed".to_owned()),
            );
        }
        let images = match self.upload_images(pick("images")).await {
            Ok(i) => i,
            Err(e) => return failed(e, None),
        };
        let req = CreateRequest {
            idempotency_key: key.clone(),
            message: SendRequest {
                client_message_id,
                text,
                images,
                files: Self::files(pick("attachments")),
            },
        };
        let created = match self.turn_calls().create_conversation(&req).await {
            Ok(c) => c.into_inner(),
            Err(e) => {
                let f = fail(e);
                let code = f.code().map(|c| c.as_str().to_owned());
                let sentence = if f.code() == Some(&ErrorCode::ViewAsNotRecorded) {
                    locale.view_as_not_recorded().to_owned()
                } else {
                    locale.create_failed().to_owned()
                };
                return failed(sentence, code);
            }
        };
        let o = self.owned();
        let view = o.view();
        let c = &created.conversation;
        let workspace = workspace_descriptor(c, &view);
        let snapshot = agent_snapshot(c, &view);
        if subscribe {
            for (revision, phase) in [
                "accepted",
                "workspace_ready",
                "agent_ready",
                "prompt_started",
                "completed",
            ]
            .iter()
            .enumerate()
            {
                self.reply(
                    "workspace.create.update",
                    &json!({"kind": "workspace", "idempotencyKey": key, "revision": revision, "phase": phase,
                            "error": null, "workspaceId": workspace["id"], "agentId": c.id,
                            "workspace": workspace, "agent": snapshot}),
                );
            }
        }
        self.reply(
            "workspace.create.response",
            &json!({"requestId": self.rid, "workspace": workspace, "agent": snapshot, "setupTerminalId": null,
                    "error": null}),
        );
    }

    async fn creation(&self) {
        let key = str_field(&self.m, "idempotencyKey").unwrap_or("").to_owned();
        let subscribe = messages::subscribe(&self.m).is_some();
        let mut payload = Map::new();
        payload.insert("requestId".into(), json!(self.rid));
        if subscribe {
            payload.insert("subscriptionId".into(), json!(Uuid::new_v4().to_string()));
        }
        let found = if key.is_empty() {
            Err(Fail::NotFound)
        } else {
            self.calls().get_creation(&key).await.map_err(fail)
        };
        match found {
            Ok(created) => {
                let o = self.owned();
                let view = o.view();
                let c = &created.conversation;
                let workspace = workspace_descriptor(c, &view);
                payload.insert(
                    "snapshot".into(),
                    json!({"kind": "workspace", "idempotencyKey": key, "revision": 4, "phase": "completed",
                           "workspaceId": workspace["id"], "agentId": c.id, "workspace": workspace,
                           "agent": agent_snapshot(c, &view), "error": null}),
                );
            }
            Err(Fail::NotFound) => {
                payload.insert("snapshot".into(), Value::Null);
            }
            Err(_) => return self.rpc_failed(self.locale().unreachable()),
        }
        payload.insert("error".into(), Value::Null);
        self.reply("creation.subscribe.response", &Value::Object(payload));
    }

    /// 7.5.6: the cancel is conversation-level and never an error row.
    async fn cancel(&self) {
        let agent = str_field(&self.m, "agentId").unwrap_or("").to_owned();
        let o = self.owned();
        let Some(id) = self.agent_id() else {
            return self.reply(
                "cancel_agent_response",
                &json!({"requestId": self.rid, "agentId": agent, "agent": null, "error": strings::NOT_FOUND}),
            );
        };
        let error = match self.calls().cancel_turn(id).await.map_err(fail) {
            Ok(_) => None,
            Err(Fail::NotFound) => Some(strings::NOT_FOUND.to_owned()),
            Err(_) => Some(o.locale.unreachable().to_owned()),
        };
        let snapshot = match self.conversation(id).await {
            Ok(c) => agent_snapshot(&c, &o.view()),
            Err(_) => Value::Null,
        };
        self.reply(
            "cancel_agent_response",
            &json!({"requestId": self.rid, "agentId": agent, "agent": snapshot, "error": error}),
        );
    }

    async fn rename(&self, id: Uuid, title: &str) -> Option<String> {
        let title: String = title
            .chars()
            .take(magpie_api::conversations::RENAME_MAX_CHARS)
            .collect();
        match self
            .calls()
            .rename_conversation(id, &RenameRequest { title })
            .await
            .map_err(fail)
        {
            Ok(_) => None,
            Err(Fail::NotFound) => Some(strings::NOT_FOUND.to_owned()),
            Err(Fail::Refused(r)) if r.code == ErrorCode::EmptyTitle => Some(self.locale().empty_title().to_owned()),
            Err(_) => Some(self.locale().unreachable().to_owned()),
        }
    }

    async fn update_agent(&self) {
        let agent = str_field(&self.m, "agentId").unwrap_or("").to_owned();
        let title = str_field(&self.m, "title").unwrap_or("").trim().to_owned();
        let error = if title.is_empty() {
            None
        } else {
            match self.agent_id() {
                None => Some(strings::NOT_FOUND.to_owned()),
                Some(id) => self.rename(id, &title).await,
            }
        };
        self.reply(
            "update_agent_response",
            &json!({"requestId": self.rid, "agentId": agent, "accepted": error.is_none(), "error": error}),
        );
    }

    async fn title_set(&self) {
        let ws = str_field(&self.m, "workspaceId").unwrap_or("").to_owned();
        let title = str_field(&self.m, "title").unwrap_or("").trim().to_owned();
        let id = ws.strip_prefix(WORKSPACE_PREFIX).and_then(|s| Uuid::parse_str(s).ok());
        let error = match id {
            None => Some(strings::NOT_FOUND.to_owned()),
            Some(_) if title.is_empty() => Some(self.locale().empty_title().to_owned()),
            Some(id) => self.rename(id, &title).await,
        };
        let shown: Option<String> = error.is_none().then(|| {
            title
                .chars()
                .take(magpie_api::conversations::RENAME_MAX_CHARS)
                .collect()
        });
        self.reply(
            "workspace.title.set.response",
            &json!({"requestId": self.rid, "workspaceId": ws, "accepted": error.is_none(), "title": shown,
                    "error": error}),
        );
    }

    fn target(&self) -> Option<Uuid> {
        if self.kind == "archive_workspace_request" {
            str_field(&self.m, "workspaceId")
                .and_then(|w| w.strip_prefix(WORKSPACE_PREFIX))
                .and_then(|s| Uuid::parse_str(s).ok())
        } else {
            self.agent_id()
        }
    }

    fn fail_sentence(&self, f: &Fail) -> String {
        match f {
            Fail::NotFound => strings::NOT_FOUND.to_owned(),
            _ => self.locale().unreachable().to_owned(),
        }
    }

    /// 7.5.8: a soft delete into Recently deleted.
    async fn archive(&self) {
        let result = match self.target() {
            None => Err(Fail::NotFound),
            Some(id) => self.calls().delete_conversation(id).await.map_err(fail),
        };
        if self.kind == "archive_workspace_request" {
            let ws = str_field(&self.m, "workspaceId").unwrap_or("");
            let (at, error) = match &result {
                Ok(c) => (json!(c.deleted_at.map_or_else(now_iso, |t| t.to_wire())), Value::Null),
                Err(f) => (Value::Null, json!(self.fail_sentence(f))),
            };
            return self.reply(
                "archive_workspace_response",
                &json!({"requestId": self.rid, "workspaceId": ws, "archivedAt": at, "error": error}),
            );
        }
        match result {
            Ok(c) => self.reply(
                "agent_archived",
                &json!({"requestId": self.rid, "agentId": c.id,
                        "archivedAt": c.deleted_at.map_or_else(now_iso, |t| t.to_wire())}),
            ),
            Err(f) => self.rpc_failed(&self.fail_sentence(&f)),
        }
    }

    /// 7.5.8: for good when already deleted, else into Recently deleted.
    async fn delete(&self) {
        let Some(id) = self.agent_id() else {
            return self.rpc_failed(strings::NOT_FOUND);
        };
        let result = match self.conversation(id).await {
            Ok(c) if c.deleted_at.is_some() => self.calls().purge_conversation(id).await.map_err(fail),
            Ok(_) => self.calls().delete_conversation(id).await.map(drop).map_err(fail),
            Err(f) => Err(f),
        };
        match result {
            Ok(()) => self.reply("agent_deleted", &json!({"requestId": self.rid, "agentId": id})),
            Err(f) => self.rpc_failed(&self.fail_sentence(&f)),
        }
    }

    /// 7.5.8: restore from Recently deleted (refused on a view-as socket).
    async fn refresh(&self) {
        let agent = str_field(&self.m, "agentId").unwrap_or("").to_owned();
        if let Some(id) = self.agent_id()
            && let Ok(c) = self.conversation(id).await
            && c.deleted_at.is_some()
        {
            if self.conn.viewer.is_some() {
                return self.reply(
                    "rpc_error",
                    &rpc_error(&self.rid, &self.kind, strings::VIEW_AS_READ_ONLY, "view_as_read_only"),
                );
            }
            if let Err(e) = self.calls().restore_conversation(id).await {
                tracing::warn!(error = %e, "restore failed");
                return self.rpc_failed(&self.fail_sentence(&fail(e)));
            }
        }
        self.reply(
            "status",
            &json!({"status": "agent_refreshed", "agentId": agent, "requestId": self.rid}),
        );
    }

    /// 7.5.9: the answer goes to magpie with its answers keyed as Claude Code reads them ([`answers_by_question`]);
    /// the protocol has no reply.
    async fn answer(&self) {
        let (Some(id), Some(ask)) = (self.agent_id(), str_field(&self.m, "requestId")) else {
            return;
        };
        let Some(mut response) = self
            .m
            .get("response")
            .and_then(|r| serde_json::from_value::<AgentPermissionResponse>(r.clone()).ok())
        else {
            tracing::warn!("ask answer without a response");
            return;
        };
        if let Some(input) = response.updated_input.as_mut() {
            answers_by_question(input);
        }
        if let Err(e) = self.calls().answer_ask(id, ask, &response).await {
            tracing::warn!(error = %e, "ask answer failed");
        }
    }

    async fn providers(&self) {
        let models = crate::http::models_value(&self.app).await;
        let cwd = str_field(&self.m, "cwd");
        self.reply(
            "get_providers_snapshot_response",
            &messages::providers_snapshot(&self.rid, cwd, models.as_ref()),
        );
    }

    async fn project_list(&self) {
        let (generation, head) = match self
            .hub
            .list(&self.app, &self.conn, None, magpie_api::conversations::LIST_MAX)
            .await
        {
            Ok(l) => (l.list.generation, l.list.head_seq),
            Err(_) => (String::new(), 0),
        };
        if messages::subscribe(&self.m).is_some() {
            self.conn.subscribe_projects();
        }
        let o = self.owned();
        self.reply(
            "project.list.response",
            &messages::project_list(&self.rid, &o.view(), &generation, head, has_sync(&self.m)),
        );
    }
}

/// The app's question card keys each `AskUserQuestion` answer by the question's `header`
/// (`buildQuestionFormAnswers`); Claude Code, the supervisor's answer message and the old stack (claudecodeui
/// `answersByQuestionText`) read them by the question's text. A key that is a header of the input's `questions` (and
/// not also a question's text) becomes that question's text; other keys stay. Rehearsal R1 (2026-11-06): an answer
/// "Blue" keyed `Color choice` reached the agent as "The user did not answer the questions."
pub fn answers_by_question(input: &mut Map<String, Value>) {
    let Some(Value::Object(answers)) = input.get("answers") else {
        return;
    };
    let questions = input
        .get("questions")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let mut out = Map::new();
    // a key that already is a question's text first: a header given beside it does not replace its answer
    for (k, v) in answers {
        if question_text(questions, k) == Some(k.as_str()) {
            out.insert(k.clone(), v.clone());
        }
    }
    for (k, v) in answers {
        let key = question_text(questions, k).unwrap_or(k.as_str());
        if !out.contains_key(key) {
            out.insert(key.to_owned(), v.clone());
        }
    }
    input.insert("answers".to_owned(), Value::Object(out));
}

/// The text of the question `key` names: the question whose text it is, else the first whose header it is.
fn question_text<'a>(questions: &'a [Value], key: &str) -> Option<&'a str> {
    let text = |q: &'a Value| q.get("question").and_then(Value::as_str);
    questions
        .iter()
        .find_map(|q| text(q).filter(|t| *t == key))
        .or_else(|| {
            questions
                .iter()
                .find(|q| q.get("header").and_then(Value::as_str) == Some(key))
                .and_then(text)
        })
}

/// A base64 text decoded 1 MiB at a time (7.5.4: never a second full copy of the image).
fn decode_slices(
    data: String,
) -> impl futures_util::Stream<Item = Result<axum::body::Bytes, std::io::Error>> + Send + 'static {
    const SLICE: usize = 1 << 20; // a multiple of 4: whole base64 quanta
    futures_util::stream::unfold((data, 0usize), |(data, pos)| async move {
        if pos >= data.len() {
            return None;
        }
        let end = (pos + SLICE).min(data.len());
        let chunk = data.get(pos..end).unwrap_or("");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(chunk.trim_end_matches(['\n', '\r']))
            .map(axum::body::Bytes::from)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e));
        Some((decoded, (data, end)))
    })
}

/// The frame of a session message (re-exported for the delegate seam's tests).
#[must_use]
pub fn frame(kind: &str, payload: &Value) -> String {
    session(kind, payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_keyed_by_header_are_keyed_by_the_question_s_text() {
        // the app's card as it answered rehearsal R1's question
        let mut input = json!({
            "questions": [
                {"question": "Do you prefer Red or Blue?", "header": "Color choice", "multiSelect": false,
                 "options": [{"label": "Red"}, {"label": "Blue"}]},
                {"question": "Why?", "header": "Reason", "options": []},
            ],
            "answers": {"Color choice": "Blue", "Why?": "calm", "elsewhere": "x"},
        })
        .as_object()
        .cloned()
        .unwrap();
        answers_by_question(&mut input);
        assert_eq!(
            input["answers"],
            json!({"Do you prefer Red or Blue?": "Blue", "Why?": "calm", "elsewhere": "x"})
        );
        // a text and its header both given: the text's answer stands
        let mut both = json!({"questions": [{"question": "Q?", "header": "H"}], "answers": {"H": "a", "Q?": "b"}})
            .as_object()
            .cloned()
            .unwrap();
        answers_by_question(&mut both);
        assert_eq!(both["answers"], json!({"Q?": "b"}));
        // no answers, or answers with no questions: unchanged
        let mut plain = json!({"plan": "1."}).as_object().cloned().unwrap();
        answers_by_question(&mut plain);
        assert_eq!(Value::Object(plain), json!({"plan": "1."}));
        let mut bare = json!({"answers": {"Which?": "A"}}).as_object().cloned().unwrap();
        answers_by_question(&mut bare);
        assert_eq!(bare["answers"], json!({"Which?": "A"}));
    }
}
