//! A small in-memory magpie for the socket tests: conversations, timeline rows with a dense row seq per epoch,
//! the person change feed, and both long-polls (M9 `after` + `waitMs`, M12 `waitMs`), behind magpie-api's own
//! router. fake-magpie scripts answers; the socket needs behaviour (rows that appear while a poll is held), so the
//! tests drive this model instead. Only the person routes pigeon's socket calls are implemented.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    clippy::struct_field_names,
    clippy::unused_async_trait_impl
)] // tests

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use aviary_core::http::ApiError;
use axum::body::Body;
use magpie_api::changes::{After, Change, ChangeBody, ChangesQuery, ChangesResponse, RemovedReason, TurnChange};
use magpie_api::conversations::{
    Ask, Conversation, CreateRequest, CreateResponse, ListPosition, ListQuery, ListResponse, ListState, Origin,
    OriginKind, RenameRequest, RunInfo, RunState, SandboxPhase, TitleSource, TurnRef,
};
use magpie_api::errors::{ErrorCode, ErrorInfo, Refusal};
use magpie_api::legacy::{
    LegacyPathRequest, Superseded, TicketDone, TicketKind, TicketOutcome, TicketRequest, TicketResponse, TicketRoute,
    WindowState,
};
use magpie_api::rows::{Prompt, PromptsResponse, Row, RowItem, TimelinePage, TimelineQuery, UserItem};
use magpie_api::server::{Ctx, Server};
use magpie_api::turns::{
    AgentPermissionResponse, CancelResponse, SendRequest, SendResponse, TurnOrigin, TurnPhase, TurnStarted,
};
use magpie_api::uploads::{UploadMeta, UploadResponse};
use magpie_api::{Admitted, Timestamp};
use tokio::sync::Notify;
use uuid::Uuid;

pub struct Conv {
    pub person: String,
    pub conv: Conversation,
    pub epoch: String,
    pub rows: Vec<Row>,
    pub next_seq: u64,
    pub sends: HashMap<String, SendResponse>,
}

#[derive(Default)]
pub struct State {
    pub convs: HashMap<Uuid, Conv>,
    pub changes: HashMap<String, Vec<Change>>,
    pub heads: HashMap<String, u64>,
    pub creations: HashMap<(String, String), CreateResponse>,
    /// One-shot refusals by route.
    pub refuse: HashMap<&'static str, VecDeque<Refusal>>,
    /// Every call: (route, person, viewer).
    pub calls: Vec<(&'static str, Option<String>, Option<String>)>,
    pub uploads: Vec<(String, usize)>,
    pub answers: Vec<(Uuid, String, AgentPermissionResponse)>,
    pub sent: Vec<(Uuid, SendRequest)>,
    /// Held answers per route: the call waits until released.
    pub hold: HashMap<&'static str, Arc<Notify>>,
    pub epochs: u64,
    /// The switch window (WP-50): `import_control.handover_allowed_at` and `rollback_at`.
    pub gate: Option<Timestamp>,
    pub rollback: Option<Timestamp>,
    /// Every ticket taken: (ticket, conversation, kind, clientMessageId, outcome).
    pub tickets: Vec<Ticket>,
    /// Conversations whose next ticket call finds them released (the fence won): `native`, and owned from then.
    pub release_at_ticket: std::collections::HashSet<Uuid>,
    /// The next send ticket call answers with `supersedes` (settle ms).
    pub supersede_next: Option<u64>,
    /// The next ticket answer's `holdMs` (else 60 000).
    pub hold_ms_next: Option<u64>,
    /// A ticket of this kind for this conversation finds it released (the fence won between a send's ticket and its
    /// copies).
    pub release_on_kind: Option<(Uuid, TicketKind)>,
    /// Upload bytes by id (`upload_raw`), and the recorded old paths.
    pub upload_bytes: HashMap<Uuid, (String, axum::body::Bytes)>,
    pub legacy_paths: Vec<(Uuid, String)>,
}

/// A ticket the model issued.
#[derive(Debug, Clone)]
pub struct Ticket {
    pub id: Uuid,
    pub conversation: Uuid,
    pub kind: TicketKind,
    pub cmid: Option<String>,
    pub outcome: Option<TicketOutcome>,
}

#[derive(Clone, Default)]
pub struct Model {
    st: Arc<Mutex<State>>,
    changed: Arc<Notify>,
}

pub const GENERATION: &str = "gen-1";

fn now() -> Timestamp {
    Timestamp::now()
}

fn refusal(code: ErrorCode, msg: &str) -> ApiError {
    Refusal::new(code, msg).into()
}

impl Model {
    pub fn st(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap()
    }

    fn bump(&self) {
        self.changed.notify_waiters();
    }

    pub fn calls_to(&self, route: &str) -> usize {
        self.st().calls.iter().filter(|c| c.0 == route).count()
    }

    pub fn refuse_once(&self, route: &'static str, r: Refusal) {
        self.st().refuse.entry(route).or_default().push_back(r);
    }

    fn record(&self, route: &'static str, ctx: &Ctx) -> Result<(), ApiError> {
        let mut st = self.st();
        st.calls
            .push((route, ctx.person.clone(), ctx.viewer.as_ref().map(|v| v.email.clone())));
        if let Some(r) = st.refuse.get_mut(route).and_then(VecDeque::pop_front) {
            return Err(r.into());
        }
        Ok(())
    }

    pub fn hold(&self, route: &'static str) -> Arc<Notify> {
        let n = Arc::new(Notify::new());
        self.st().hold.insert(route, n.clone());
        n
    }

    async fn held(&self, route: &'static str) {
        let h = self.st().hold.get(route).cloned();
        if let Some(h) = h {
            h.notified().await;
            self.st().hold.remove(route);
        }
    }

    fn push_change(st: &mut State, person: &str, conversation: Uuid, body: ChangeBody) -> u64 {
        let head = st.heads.entry(person.to_owned()).or_insert(0);
        *head += 1;
        let seq = *head;
        st.changes.entry(person.to_owned()).or_default().push(Change {
            seq,
            at: now(),
            conversation_id: conversation,
            body,
        });
        seq
    }

    fn upsert(st: &mut State, id: Uuid) {
        let (person, conv) = {
            let c = &st.convs[&id];
            (c.person.clone(), c.conv.clone())
        };
        Self::push_change(
            st,
            &person,
            id,
            ChangeBody::ConversationUpserted {
                conversation: Box::new(conv),
            },
        );
    }

    pub fn conversation(id: Uuid, title: Option<&str>) -> Conversation {
        let t = now();
        Conversation {
            id,
            title: title.map(str::to_owned),
            title_source: title.map(|_| TitleSource::User),
            created_at: t,
            updated_at: t,
            last_user_message_at: None,
            deleted_at: None,
            purge_at: None,
            claude_session_id: Some(id.to_string()),
            turn: None,
            last_turn: None,
            sandbox: None,
            asks: Vec::new(),
            migrating: false,
            run: RunInfo {
                state: RunState::Idle,
                background: Vec::new(),
            },
            model: None,
            context_tokens: None,
            origin: Origin {
                kind: OriginKind::New,
                share_id: None,
            },
        }
    }

    /// A conversation of `person` (emits its upsert).
    pub fn add(&self, person: &str, title: Option<&str>) -> Uuid {
        let id = Uuid::new_v4();
        let mut st = self.st();
        st.epochs += 1;
        let epoch = format!("e{}", st.epochs);
        st.convs.insert(
            id,
            Conv {
                person: person.to_owned(),
                conv: Self::conversation(id, title),
                epoch,
                rows: Vec::new(),
                next_seq: 1,
                sends: HashMap::new(),
            },
        );
        Self::upsert(&mut st, id);
        drop(st);
        self.bump();
        id
    }

    /// A legacy conversation's writes answer 409 `route: "legacy"` (magpie's migrating table).
    fn not_legacy(&self, id: Uuid) -> Result<(), ApiError> {
        if self.st().convs.get(&id).is_some_and(|c| c.conv.migrating) {
            return Err(Refusal::legacy_route().into());
        }
        Ok(())
    }

    /// Sets a conversation's owner (`true`: the old stack's) with its list change, as the importer's release or
    /// reverse hand-over does.
    pub fn set_migrating(&self, id: Uuid, migrating: bool) {
        let mut st = self.st();
        st.convs.get_mut(&id).unwrap().conv.migrating = migrating;
        Self::upsert(&mut st, id);
        drop(st);
        self.bump();
    }

    /// A legacy conversation of `person` (as the bulk import makes it).
    pub fn add_legacy(&self, person: &str, title: Option<&str>) -> Uuid {
        let id = self.add(person, title);
        self.set_migrating(id, true);
        id
    }

    /// The importer's release: only with no open ticket (else `false`, nothing changes).
    pub fn try_release(&self, id: Uuid) -> bool {
        let mut st = self.st();
        if st.tickets.iter().any(|t| t.conversation == id && t.outcome.is_none()) {
            return false;
        }
        st.convs.get_mut(&id).unwrap().conv.migrating = false;
        Self::upsert(&mut st, id);
        drop(st);
        self.bump();
        true
    }

    /// The tickets of a conversation.
    pub fn tickets_of(&self, id: Uuid) -> Vec<Ticket> {
        self.st()
            .tickets
            .iter()
            .filter(|t| t.conversation == id)
            .cloned()
            .collect()
    }

    pub fn with_conv<R>(&self, id: Uuid, f: impl FnOnce(&mut Conv) -> R) -> R {
        let mut st = self.st();
        let r = f(st.convs.get_mut(&id).unwrap());
        drop(st);
        self.bump();
        r
    }

    /// Edits the conversation and emits its upsert.
    pub fn edit(&self, id: Uuid, f: impl FnOnce(&mut Conversation)) {
        let mut st = self.st();
        f(&mut st.convs.get_mut(&id).unwrap().conv);
        Self::upsert(&mut st, id);
        drop(st);
        self.bump();
    }

    /// A new row (the next row seq).
    pub fn row(&self, id: Uuid, turn: Option<Uuid>, item: RowItem) -> u64 {
        self.with_conv(id, |c| {
            let seq = c.next_seq;
            c.next_seq += 1;
            c.rows.push(Row {
                seq_start: seq,
                seq_end: seq,
                seqs: vec![seq],
                updated: false,
                timestamp: now(),
                turn_id: turn,
                item,
            });
            seq
        })
    }

    /// Updates the row that started at `seq_start` (it takes the next row seq).
    pub fn update(&self, id: Uuid, seq_start: u64, f: impl FnOnce(&mut RowItem)) -> u64 {
        self.with_conv(id, |c| {
            let seq = c.next_seq;
            c.next_seq += 1;
            let row = c.rows.iter_mut().find(|r| r.seq_start == seq_start).unwrap();
            f(&mut row.item);
            row.seq_end = seq;
            row.seqs.push(seq);
            row.updated = true;
            seq
        })
    }

    pub fn max_seq(&self, id: Uuid) -> u64 {
        self.st().convs[&id].next_seq - 1
    }

    pub fn epoch(&self, id: Uuid) -> String {
        self.st().convs[&id].epoch.clone()
    }

    /// Starts a turn (the `turn` change and the upsert).
    pub fn start_turn(&self, id: Uuid, origin: TurnOrigin) -> Uuid {
        let turn = Uuid::new_v4();
        let mut st = self.st();
        Self::start_turn_in(&mut st, id, turn, origin);
        drop(st);
        self.bump();
        turn
    }

    fn start_turn_in(st: &mut State, id: Uuid, turn: Uuid, origin: TurnOrigin) {
        let c = st.convs.get_mut(&id).unwrap();
        c.conv.turn = Some(TurnRef {
            id: turn,
            started_at: now(),
            origin,
        });
        c.conv.run.state = RunState::Running;
        let person = c.person.clone();
        let timeline_seq = c.next_seq - 1;
        Self::push_change(
            st,
            &person,
            id,
            ChangeBody::Turn(TurnChange {
                turn_id: turn,
                phase: TurnPhase::Started,
                origin,
                error: None,
                timeline_seq,
            }),
        );
        Self::upsert(st, id);
    }

    /// Ends the running turn.
    pub fn end_turn(&self, id: Uuid, phase: TurnPhase, error: Option<ErrorInfo>) {
        let mut st = self.st();
        Self::end_turn_in(&mut st, id, phase, error);
        drop(st);
        self.bump();
    }

    fn end_turn_in(st: &mut State, id: Uuid, phase: TurnPhase, error: Option<ErrorInfo>) {
        let c = st.convs.get_mut(&id).unwrap();
        let Some(t) = c.conv.turn.take() else { return };
        c.conv.run.state = RunState::Idle;
        let person = c.person.clone();
        let timeline_seq = c.next_seq - 1;
        Self::push_change(
            st,
            &person,
            id,
            ChangeBody::Turn(TurnChange {
                turn_id: t.id,
                phase,
                origin: t.origin,
                error,
                timeline_seq,
            }),
        );
        Self::upsert(st, id);
    }

    /// A change of any other kind.
    pub fn change(&self, id: Uuid, body: ChangeBody) -> u64 {
        let mut st = self.st();
        let person = st.convs[&id].person.clone();
        let seq = Self::push_change(&mut st, &person, id, body);
        drop(st);
        self.bump();
        seq
    }

    pub fn sandbox(&self, id: Uuid, phase: SandboxPhase) {
        self.edit(id, |c| c.sandbox = Some(phase));
        self.change(id, ChangeBody::Sandbox { state: phase });
    }

    pub fn open_ask(&self, id: Uuid, ask: Ask) {
        let a = ask.clone();
        self.edit(id, |c| c.asks.push(a));
        self.change(id, ChangeBody::Ask { ask });
    }

    /// Rebuilds the rows in a new epoch (renumbered from 1) and announces it.
    pub fn rewrite(&self, id: Uuid) -> String {
        let epoch = self.with_conv(id, |c| {
            let mut n = 0;
            for r in &mut c.rows {
                n += 1;
                r.seq_start = n;
                r.seq_end = n;
                r.seqs = vec![n];
                r.updated = false;
            }
            c.next_seq = n + 1;
            c.epoch = format!("{}-r", c.epoch);
            c.epoch.clone()
        });
        self.change(id, ChangeBody::TimelineReplaced { epoch: epoch.clone() });
        epoch
    }

    fn mine(&self, ctx: &Ctx, id: Uuid) -> Result<(), ApiError> {
        let st = self.st();
        match st.convs.get(&id) {
            Some(c) if Some(&c.person) == ctx.person.as_ref() => Ok(()),
            _ => Err(refusal(ErrorCode::NotFound, "not found")),
        }
    }

    fn read_only(ctx: &Ctx) -> Result<(), ApiError> {
        if ctx.viewer.is_some() {
            return Err(refusal(ErrorCode::ViewAsReadOnly, "read only"));
        }
        Ok(())
    }

    fn page(c: &Conv, q: &TimelineQuery) -> TimelinePage {
        let limit = q.limit.unwrap_or(40) as usize;
        let mut rows: Vec<Row> = if let Some(after) = q.after {
            let mut v: Vec<Row> = c.rows.iter().filter(|r| r.seq_end > after).cloned().collect();
            v.sort_by_key(|r| r.seq_end);
            v.truncate(limit);
            v
        } else if let Some(before) = q.before {
            let v: Vec<Row> = c.rows.iter().filter(|r| r.seq_start < before).cloned().collect();
            let skip = v.len().saturating_sub(limit);
            v.into_iter().skip(skip).collect()
        } else {
            let skip = c.rows.len().saturating_sub(limit);
            c.rows.iter().skip(skip).cloned().collect()
        };
        if q.after.is_none() {
            rows.sort_by_key(|r| r.seq_start);
        }
        let min = c.rows.first().map_or(0, |r| r.seq_start);
        let max = c.next_seq - 1;
        let has_older = rows.first().is_some_and(|r| r.seq_start > min);
        let has_newer = rows.last().is_some_and(|r| r.seq_end < max) && q.after.is_some();
        TimelinePage {
            epoch: c.epoch.clone(),
            min_seq: min,
            max_seq: max,
            has_older,
            has_newer,
            rows,
            live: None,
        }
    }
}

impl Server for Model {
    async fn list_conversations(&self, ctx: Ctx, query: ListQuery) -> Result<ListResponse, ApiError> {
        self.record("list_conversations", &ctx)?;
        self.held("list_conversations").await;
        let st = self.st();
        let person = ctx.person.clone().unwrap_or_default();
        let deleted = query.state == Some(ListState::Deleted);
        let mut all: Vec<&Conv> = st
            .convs
            .values()
            .filter(|c| c.person == person && c.conv.deleted_at.is_some() == deleted)
            .filter(|c| {
                query.search.as_deref().is_none_or(|s| {
                    c.conv
                        .title
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&s.to_lowercase())
                })
            })
            .collect();
        all.sort_by(|a, b| {
            b.conv
                .updated_at
                .0
                .cmp(&a.conv.updated_at.0)
                .then(b.conv.id.cmp(&a.conv.id))
        });
        let start: usize = query.cursor.as_deref().map_or(0, |c| c.parse().unwrap());
        let limit = query.limit.unwrap_or(200) as usize;
        let page: Vec<Conversation> = all.iter().skip(start).take(limit).map(|c| c.conv.clone()).collect();
        let next = (start + limit < all.len()).then(|| (start + limit).to_string());
        Ok(ListResponse {
            conversations: page,
            next_cursor: next,
            list: ListPosition {
                generation: GENERATION.into(),
                head_seq: st.heads.get(&person).copied().unwrap_or(0),
            },
        })
    }

    async fn get_conversation(&self, ctx: Ctx, id: Uuid) -> Result<Conversation, ApiError> {
        self.record("get_conversation", &ctx)?;
        self.mine(&ctx, id)?;
        Ok(self.st().convs[&id].conv.clone())
    }

    async fn create_conversation(&self, ctx: Ctx, body: CreateRequest) -> Result<Admitted<CreateResponse>, ApiError> {
        self.record("create_conversation", &ctx)?;
        let person = ctx.person.clone().unwrap_or_default();
        let key = (person.clone(), body.idempotency_key.clone());
        if let Some(r) = self.st().creations.get(&key).cloned() {
            return Ok(Admitted::Repeat(r));
        }
        let id = self.add(&person, None);
        let turn = if body.message.text.is_empty() {
            None
        } else {
            self.row(
                id,
                None,
                RowItem::User(UserItem {
                    message_id: body.message.client_message_id.clone(),
                    client_message_id: Some(body.message.client_message_id.clone()),
                    text: body.message.text.clone(),
                    images: Vec::new(),
                    files: Vec::new(),
                    viewer: None,
                }),
            );
            Some(self.start_turn(id, TurnOrigin::Person))
        };
        let conv = self.st().convs[&id].conv.clone();
        let r = CreateResponse {
            conversation: conv,
            turn: turn.map(|t| TurnStarted {
                id: t,
                started_at: now(),
            }),
        };
        self.st().creations.insert(key, r.clone());
        Ok(Admitted::New(r))
    }

    async fn get_creation(&self, ctx: Ctx, idempotency_key: String) -> Result<CreateResponse, ApiError> {
        self.record("get_creation", &ctx)?;
        let person = ctx.person.clone().unwrap_or_default();
        self.st()
            .creations
            .get(&(person, idempotency_key))
            .cloned()
            .ok_or_else(|| refusal(ErrorCode::NotFound, "not found"))
    }

    async fn rename_conversation(&self, ctx: Ctx, id: Uuid, body: RenameRequest) -> Result<Conversation, ApiError> {
        self.record("rename_conversation", &ctx)?;
        Self::read_only(&ctx)?;
        self.mine(&ctx, id)?;
        self.not_legacy(id)?;
        if body.title.trim().is_empty() {
            return Err(refusal(ErrorCode::EmptyTitle, "empty"));
        }
        self.edit(id, |c| {
            c.title = Some(body.title.trim().to_owned());
            c.title_source = Some(TitleSource::User);
        });
        Ok(self.st().convs[&id].conv.clone())
    }

    async fn delete_conversation(&self, ctx: Ctx, id: Uuid) -> Result<Conversation, ApiError> {
        self.record("delete_conversation", &ctx)?;
        Self::read_only(&ctx)?;
        self.mine(&ctx, id)?;
        self.not_legacy(id)?;
        self.edit(id, |c| c.deleted_at = Some(now()));
        Ok(self.st().convs[&id].conv.clone())
    }

    async fn restore_conversation(&self, ctx: Ctx, id: Uuid) -> Result<Conversation, ApiError> {
        self.record("restore_conversation", &ctx)?;
        Self::read_only(&ctx)?;
        self.mine(&ctx, id)?;
        self.not_legacy(id)?;
        self.edit(id, |c| c.deleted_at = None);
        Ok(self.st().convs[&id].conv.clone())
    }

    async fn purge_conversation(&self, ctx: Ctx, id: Uuid) -> Result<(), ApiError> {
        self.record("purge_conversation", &ctx)?;
        Self::read_only(&ctx)?;
        self.mine(&ctx, id)?;
        self.not_legacy(id)?;
        let mut st = self.st();
        let c = st.convs.remove(&id).unwrap();
        Self::push_change(
            &mut st,
            &c.person,
            id,
            ChangeBody::ConversationRemoved {
                reason: RemovedReason::Purged,
            },
        );
        drop(st);
        self.bump();
        Ok(())
    }

    async fn send_turn(&self, ctx: Ctx, id: Uuid, body: SendRequest) -> Result<Admitted<SendResponse>, ApiError> {
        self.record("send_turn", &ctx)?;
        self.held("send_turn").await;
        self.mine(&ctx, id)?;
        self.not_legacy(id)?;
        let mut st = self.st();
        st.sent.push((id, body.clone()));
        let c = st.convs.get_mut(&id).unwrap();
        if let Some(r) = c.sends.get(&body.client_message_id) {
            return Ok(Admitted::Repeat(r.clone()));
        }
        if c.conv.deleted_at.is_some() {
            return Err(refusal(ErrorCode::ConversationDeleted, "deleted"));
        }
        if c.conv.turn.is_some() {
            return Err(refusal(ErrorCode::TurnRunning, "running"));
        }
        let seq = c.next_seq;
        c.next_seq += 1;
        c.rows.push(Row {
            seq_start: seq,
            seq_end: seq,
            seqs: vec![seq],
            updated: false,
            timestamp: now(),
            turn_id: None,
            item: RowItem::User(UserItem {
                message_id: body.client_message_id.clone(),
                client_message_id: Some(body.client_message_id.clone()),
                text: body.text.clone(),
                images: Vec::new(),
                files: Vec::new(),
                viewer: ctx.viewer.as_ref().map(|v| v.email.clone()),
            }),
        });
        let turn = Uuid::new_v4();
        let r = SendResponse {
            turn: TurnStarted {
                id: turn,
                started_at: now(),
            },
        };
        c.sends.insert(body.client_message_id.clone(), r.clone());
        Self::start_turn_in(&mut st, id, turn, TurnOrigin::Person);
        drop(st);
        self.bump();
        Ok(Admitted::New(r))
    }

    async fn cancel_turn(&self, ctx: Ctx, id: Uuid) -> Result<CancelResponse, ApiError> {
        self.record("cancel_turn", &ctx)?;
        self.mine(&ctx, id)?;
        self.not_legacy(id)?;
        let turn = self.st().convs[&id].conv.turn.as_ref().map(|t| t.id);
        if turn.is_some() {
            self.end_turn(id, TurnPhase::Canceled, None);
        }
        Ok(CancelResponse { turn_id: turn })
    }

    async fn timeline(&self, ctx: Ctx, id: Uuid, query: TimelineQuery) -> Result<TimelinePage, ApiError> {
        self.record("timeline", &ctx)?;
        self.mine(&ctx, id)?;
        let wait = Duration::from_millis(query.wait_ms.unwrap_or(0).min(25_000));
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notified = self.changed.notified();
            {
                let st = self.st();
                let Some(c) = st.convs.get(&id) else {
                    return Err(refusal(ErrorCode::NotFound, "gone"));
                };
                let page = Self::page(c, &query);
                if query.after.is_none() || !page.rows.is_empty() || wait.is_zero() {
                    return Ok(page);
                }
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                let st = self.st();
                return Ok(Self::page(&st.convs[&id], &query));
            }
        }
    }

    async fn prompts(&self, ctx: Ctx, id: Uuid) -> Result<PromptsResponse, ApiError> {
        self.record("prompts", &ctx)?;
        self.mine(&ctx, id)?;
        let st = self.st();
        let c = &st.convs[&id];
        Ok(PromptsResponse {
            epoch: c.epoch.clone(),
            prompts: c
                .rows
                .iter()
                .filter_map(|r| match &r.item {
                    RowItem::User(u) => Some(Prompt {
                        seq: r.seq_start,
                        timestamp: r.timestamp,
                        preview: u.text.chars().take(200).collect(),
                    }),
                    _ => None,
                })
                .collect(),
        })
    }

    async fn answer_ask(
        &self,
        ctx: Ctx,
        id: Uuid,
        ask_id: String,
        body: AgentPermissionResponse,
    ) -> Result<(), ApiError> {
        self.record("answer_ask", &ctx)?;
        Self::read_only(&ctx)?;
        self.mine(&ctx, id)?;
        self.st().answers.push((id, ask_id, body));
        Ok(())
    }

    async fn changes(&self, ctx: Ctx, query: ChangesQuery) -> Result<ChangesResponse, ApiError> {
        self.record("changes", &ctx)?;
        self.held("changes").await;
        let person = ctx.person.clone().unwrap_or_default();
        let wait = Duration::from_millis(query.wait_ms.unwrap_or(0).min(25_000));
        let deadline = tokio::time::Instant::now() + wait;
        let limit = query.limit.unwrap_or(500) as usize;
        loop {
            let notified = self.changed.notified();
            {
                let st = self.st();
                let head = st.heads.get(&person).copied().unwrap_or(0);
                let after = match query.after {
                    After::Latest => {
                        return Ok(ChangesResponse {
                            generation: GENERATION.into(),
                            head_seq: head,
                            changes: Vec::new(),
                        });
                    }
                    After::Seq(n) => n,
                };
                let changes: Vec<Change> = st
                    .changes
                    .get(&person)
                    .map(|v| v.iter().filter(|c| c.seq > after).take(limit).cloned().collect())
                    .unwrap_or_default();
                if !changes.is_empty() || wait.is_zero() {
                    return Ok(ChangesResponse {
                        generation: GENERATION.into(),
                        head_seq: head,
                        changes,
                    });
                }
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                let head = self.st().heads.get(&person).copied().unwrap_or(0);
                return Ok(ChangesResponse {
                    generation: GENERATION.into(),
                    head_seq: head,
                    changes: Vec::new(),
                });
            }
        }
    }

    async fn upload(&self, ctx: Ctx, meta: UploadMeta, body: Body) -> Result<UploadResponse, ApiError> {
        self.record("upload", &ctx)?;
        let bytes = axum::body::to_bytes(body, 200 << 20).await.unwrap();
        self.st().uploads.push((meta.content_type.clone(), bytes.len()));
        let id = Uuid::new_v4();
        self.st()
            .upload_bytes
            .insert(id, (meta.content_type.clone(), bytes.clone()));
        Ok(UploadResponse {
            upload_id: id,
            path: format!("uploads/{id}/{}", meta.name),
            name: meta.name,
            size: bytes.len() as u64,
            mime_type: meta.content_type,
            width: None,
            height: None,
        })
    }

    async fn revoke_share(&self, ctx: Ctx, _share_id: String) -> Result<(), ApiError> {
        self.record("revoke_share", &ctx)?;
        Err(refusal(ErrorCode::NotFound, "not found"))
    }

    async fn legacy_window(&self, ctx: Ctx) -> Result<WindowState, ApiError> {
        self.record("legacy_window", &ctx)?;
        Ok(self.window(&ctx))
    }

    async fn legacy_ticket(&self, ctx: Ctx, id: Uuid, body: TicketRequest) -> Result<TicketResponse, ApiError> {
        self.record("legacy_ticket", &ctx)?;
        self.held("legacy_ticket").await;
        self.ticket(&ctx, id, &body)
    }

    async fn legacy_ticket_done(&self, ctx: Ctx, ticket: Uuid, body: TicketDone) -> Result<(), ApiError> {
        self.record("legacy_ticket_done", &ctx)?;
        let mut st = self.st();
        let Some(t) = st.tickets.iter_mut().find(|t| t.id == ticket) else {
            return Err(refusal(ErrorCode::NotFound, "not found"));
        };
        if t.outcome.is_none() && t.kind.closes_on(body.outcome) {
            t.outcome = Some(body.outcome);
        }
        Ok(())
    }

    async fn upload_legacy_path(&self, ctx: Ctx, upload_id: Uuid, body: LegacyPathRequest) -> Result<(), ApiError> {
        self.record("upload_legacy_path", &ctx)?;
        self.st().legacy_paths.push((upload_id, body.legacy_path));
        Ok(())
    }

    async fn upload_raw(&self, ctx: Ctx, upload_id: Uuid) -> Result<axum::response::Response, ApiError> {
        self.record("upload_raw", &ctx)?;
        let Some((mime, bytes)) = self.st().upload_bytes.get(&upload_id).cloned() else {
            return Err(refusal(ErrorCode::NotFound, "not found"));
        };
        Ok(axum::response::Response::builder()
            .header("content-type", mime)
            .body(Body::from(bytes))
            .unwrap())
    }
}

impl Model {
    fn window(&self, ctx: &Ctx) -> WindowState {
        let st = self.st();
        let person = ctx.person.clone().unwrap_or_default();
        WindowState {
            handover_allowed_at: st.gate,
            rollback_at: st.rollback,
            any_legacy: st.convs.values().any(|c| c.conv.migrating),
            person_legacy: st
                .convs
                .values()
                .filter(|c| c.person == person && c.conv.migrating)
                .count() as u64,
        }
    }

    fn ticket(&self, ctx: &Ctx, id: Uuid, body: &TicketRequest) -> Result<TicketResponse, ApiError> {
        let mut st = self.st();
        match st.convs.get(&id) {
            Some(c) if Some(&c.person) == ctx.person.as_ref() => {}
            _ => return Err(refusal(ErrorCode::NotFound, "not found")),
        }
        if body.client_message_id.is_some() && body.kind != TicketKind::Send {
            return Err(refusal(ErrorCode::BadRequest, "only a send carries a clientMessageId"));
        }
        let on_kind = st.release_on_kind.is_some_and(|(c, k)| c == id && k == body.kind);
        if on_kind {
            st.release_on_kind = None;
        }
        if st.release_at_ticket.remove(&id) || on_kind {
            st.convs.get_mut(&id).unwrap().conv.migrating = false;
            Self::upsert(&mut st, id);
        }
        if !st.convs[&id].conv.migrating {
            drop(st);
            self.bump();
            return Ok(TicketResponse {
                route: TicketRoute::Native,
                ticket: None,
                issued_at: None,
                hold_ms: None,
                supersedes: None,
                reused: false,
            });
        }
        let supersedes = if body.kind == TicketKind::Send {
            st.supersede_next.take().and_then(|settle_ms| {
                let open = st
                    .tickets
                    .iter()
                    .rev()
                    .find(|t| t.conversation == id && t.cmid == body.client_message_id && t.outcome.is_none())?;
                Some(Superseded {
                    ticket: open.id,
                    settle_ms,
                })
            })
        } else {
            None
        };
        let t = Ticket {
            id: Uuid::new_v4(),
            conversation: id,
            kind: body.kind,
            cmid: body.client_message_id.clone(),
            outcome: None,
        };
        let ticket = t.id;
        st.tickets.push(t);
        let hold_ms = st.hold_ms_next.take().unwrap_or(60_000);
        Ok(TicketResponse {
            route: TicketRoute::Legacy,
            ticket: Some(ticket),
            issued_at: Some(now()),
            hold_ms: Some(hold_ms),
            supersedes,
            reused: false,
        })
    }
}

impl Model {
    /// Waits until the ticket list satisfies `pred` (outcomes are reported in the background).
    pub async fn wait_tickets(&self, pred: impl Fn(&[Ticket]) -> bool) {
        for _ in 0..500 {
            if pred(&self.st().tickets) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("tickets never matched: {:?}", self.st().tickets);
    }
}

/// Serves the model on a local port.
pub async fn serve(model: &Model) -> aviary_fakes::Served {
    aviary_fakes::serve(magpie_api::server::router(Arc::new(model.clone())))
        .await
        .unwrap()
}
