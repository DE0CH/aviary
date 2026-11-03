//! The switch window (DESIGN 9.2-9.5; switch-final 4.1, 3.2 C4-C5, 11, 23 C17): pigeon fronts the conversations
//! the old stack still owns, with nothing read-only, cut or lost. Deleted in A6 with the window's magpie routes
//! (DESIGN 9.7), except the request-free-first drain, which lives in `feeds` and stays.
//!
//! - **Ownership.** magpie's conversation row says who owns a conversation (`migrating`: the old stack). pigeon keeps
//!   what it last read per conversation ([`Legacy::observe`], fed by every list answer, conversation read and person
//!   change), and asks magpie for a conversation it has not seen while the person has legacy conversations. A
//!   native write that magpie answers 409 `conversation_migrating` with `route: "legacy"` is sent again here (the
//!   re-route). The installed [`feeds::Owners`] hands a legacy conversation's socket requests and timeline
//!   subscriptions to the legacy delegate.
//! - **The legacy upstream (LU).** One WebSocket per native socket to the old router's `/paseo-ws` through the
//!   bridge Service [`LU_BASE`] (DESIGN 15.5 B1), opened the first time that socket touches a legacy conversation, as
//!   the person with the newest Access token pigeon verified for them ([`tokens::Book`]), with pigeon's pair token in
//!   [`LU_HEADER`] (the old router counts the socket as LU only then and strips the header, switch-final C5). A view's
//!   socket carries the same view claims signed with the old router's key ([`OLD_VIEW_AS_HEADER`]). It carries the
//!   conversation's timeline, live stream and writes ([`lu`]); a write goes under a magpie ticket that closes only on
//!   the old stack's outcome ([`delegate`]). An LU that drops while its tab stays is reopened only with a token that
//!   lives at least [`REOPEN_MIN_LIFE`] more; otherwise the tab is closed 1012 at a request-free moment and reconnects
//!   through the edge, which checks the Access session itself. An LU answered 401 gives the tab pigeon's signed-out
//!   answer (4401 on the socket, 401 `NOT_SIGNED_IN` on HTTP), never a write error (`pigeon_lu_401_total`).
//! - **The owner changes.** At a release (legacy → owned) every socket watching the conversation gets one
//!   `agent.timeline.replacement` in magpie's epoch and the native feed from then ([`crate::feeds::Hub::
//!   owner_changed`]); at a reverse hand-over (owned → legacy, a rollback) one replacement in the old child's epoch
//!   and the LU feed. None before.
//! - **Uploads** ([`uploads`]): under the old route's limits while any conversation is legacy; from T0 to the gate
//!   (and during a rollback) teed into the old child and recorded in magpie with the old path; from the gate copied
//!   into the old child at send time, under the send's ticket and one upload ticket per file.
//! - **The rest over LU HTTP** ([`upstream`]): a legacy conversation's shares (create and revoke under tickets), its
//!   files, its warm calls before the gate (a ticketed write; from the gate a warm is a hermit hint only).
//! - **The move's input** ([`activity`]): `GET /internal/switch/activity` on the ops port, the last warm call and
//!   upload per person.

pub mod activity;
pub(crate) mod delegate;
pub mod lu;
pub mod rest;
pub mod tokens;
pub mod uploads;
pub mod upstream;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use magpie_api::conversations::Conversation;
use magpie_api::legacy::WindowState;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::registry::Registry;
use uuid::Uuid;

use crate::feeds::{Delegate, Owners};
use crate::paseo::transport::Conn;
use crate::{App, Secrets, Tuning, Upstreams};

/// The old ui's front on the new cluster: the bridge Service of DESIGN 15.5 B1.
pub const LU_BASE: &str = "http://ui-legacy:8080";
/// The header that marks an upgrade or request as pigeon's legacy upstream; its value is pigeon's pair token.
pub const LU_HEADER: &str = "x-aviary-lu";
/// The header the old router reads a view-as token from.
pub const OLD_VIEW_AS_HEADER: &str = "x-jarvis-view-as";
/// The old router's Paseo socket.
pub const OLD_SOCKET: &str = "/paseo-ws";
/// The old upload route (one or more `files` parts; 30 MB per file, 10 files).
pub const OLD_UPLOAD: &str = "/api/assets/files";
/// The old warm route.
pub const OLD_WARM: &str = "/api/jarvis/warm";
/// The old route's per-file cap (the old `assets.routes.ts`): every upload keeps it while any conversation is
/// legacy, so a file either stack accepted is acceptable to both (DESIGN 9.5).
pub const OLD_FILE_MAX: u64 = 30 * 1024 * 1024;
/// The old timeline's id of a user message (`paseo-hub.ts`: `paseo-user-<clientMessageId>`).
pub const OLD_USER_ROW_PREFIX: &str = "paseo-user-";
/// An LU that dropped is reopened on the same tab only with a token that lives this much longer.
pub const REOPEN_MIN_LIFE: Duration = Duration::from_hours(1);
/// The LU upgrade and its hello (a cold old child starts within it).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// A read through LU.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Who serves a conversation, as pigeon last read it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    person: String,
    legacy: bool,
    deleted: bool,
}

/// The window's counters of DESIGN 9.9 that pigeon owns (each a zero counter but the replacements).
#[derive(Debug, Clone)]
pub struct Metrics {
    /// `pigeon_switch_write_refused_total`: a write refused or dropped because of the switch.
    pub write_refused: Counter,
    /// `pigeon_lu_errors_total`: an LU call that failed (an answer the tab then got as an error).
    pub lu_errors: Counter,
    /// `pigeon_lu_503_total`: an LU upgrade or call the old router answered 503.
    pub lu_503: Counter,
    /// `pigeon_lu_401_total`: an LU upgrade or call answered 401 (the tab got the signed-out answer).
    pub lu_401: Counter,
    /// `pigeon_upload_tee_failed_total`: a teed upload the old child refused or that failed.
    pub tee_failed: Counter,
    /// `pigeon_legacy_replacements_total`: replacements sent at an owner change (counted, at most one per socket).
    pub replacements: Counter,
    /// `pigeon_lu_sockets`: LU sockets open on this replica.
    pub lu_sockets: Gauge,
}

impl Metrics {
    fn register(r: &mut Registry) -> Self {
        let m = Self {
            write_refused: Counter::default(),
            lu_errors: Counter::default(),
            lu_503: Counter::default(),
            lu_401: Counter::default(),
            tee_failed: Counter::default(),
            replacements: Counter::default(),
            lu_sockets: Gauge::default(),
        };
        r.register(
            "switch_write_refused",
            "Writes refused or dropped because of the switch (zero)",
            m.write_refused.clone(),
        );
        r.register(
            "lu_errors",
            "Legacy upstream calls that failed (zero)",
            m.lu_errors.clone(),
        );
        r.register("lu_503", "Legacy upstream answers 503 (zero)", m.lu_503.clone());
        r.register(
            "lu_401",
            "Legacy upstream answers 401: the tab got the signed-out answer",
            m.lu_401.clone(),
        );
        r.register(
            "upload_tee_failed",
            "Teed uploads that failed (zero)",
            m.tee_failed.clone(),
        );
        r.register(
            "legacy_replacements",
            "Timeline replacements sent at an owner change",
            m.replacements.clone(),
        );
        r.register("lu_sockets", "Legacy upstream sockets open", m.lu_sockets.clone());
        m
    }
}

/// The switch window's state of this replica. Cheap to clone.
#[derive(Clone)]
pub struct Legacy(pub(crate) Arc<Inner>);

impl std::fmt::Debug for Legacy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Legacy").finish_non_exhaustive()
    }
}

pub(crate) struct Inner {
    pub(crate) base: String,
    pub(crate) pair: String,
    pub(crate) user_agent: String,
    /// LU HTTP (each call bounds itself).
    pub(crate) http: reqwest::Client,
    pub(crate) tokens: tokens::Book,
    owners: Mutex<HashMap<Uuid, Entry>>,
    windows: Mutex<HashMap<String, (Instant, WindowState)>>,
    window_ttl: Duration,
    pub(crate) heartbeat: Duration,
    lus: Mutex<HashMap<Uuid, Arc<lu::Lu>>>,
    pub(crate) activity: activity::Activity,
    pub(crate) lu_shares: rest::LuShares,
    pub(crate) metrics: Metrics,
    app: OnceLock<App>,
    delegate: OnceLock<Arc<dyn Delegate>>,
}

/// The installed [`Owners`]: a conversation pigeon last read as legacy goes to the legacy delegate.
struct LegacyOwners(Legacy);

impl Owners for LegacyOwners {
    fn delegate(&self, person: &str, conversation: &Uuid) -> Option<Arc<dyn Delegate>> {
        if self.0.is_legacy(person, conversation) == Some(true) {
            self.0.0.delegate.get().cloned()
        } else {
            None
        }
    }
}

impl Legacy {
    pub(crate) fn new(
        build: &aviary_core::version::Build,
        upstreams: &Upstreams,
        secrets: &Secrets,
        tuning: &Tuning,
        registry: &mut Registry,
    ) -> Self {
        let http = aviary_core::http::client_builder(build, None)
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        let inner = Arc::new(Inner {
            base: upstreams.legacy.trim_end_matches('/').to_owned(),
            pair: secrets.token_ui.clone(),
            user_agent: format!("pigeon/{}", aviary_core::version::sha12()),
            http,
            tokens: tokens::Book::new(secrets.legacy_view_as_key.clone()),
            owners: Mutex::new(HashMap::new()),
            windows: Mutex::new(HashMap::new()),
            window_ttl: tuning.legacy_window,
            heartbeat: tuning.legacy_heartbeat,
            lus: Mutex::new(HashMap::new()),
            activity: activity::Activity::default(),
            lu_shares: rest::LuShares::default(),
            metrics: Metrics::register(registry),
            app: OnceLock::new(),
            delegate: OnceLock::new(),
        });
        let l = Self(inner);
        let _ = l.0.delegate.set(Arc::new(delegate::LegacyDelegate(l.clone())));
        l
    }

    pub(crate) fn set_app(&self, app: &App) {
        let _ = self.0.app.set(app.clone());
        self.0.tokens.set_app(app);
    }

    pub(crate) fn app(&self) -> Option<&App> {
        self.0.app.get()
    }

    /// The [`Owners`] the hub of [`App::new`] routes by.
    #[must_use]
    pub fn owners(&self) -> Arc<dyn Owners> {
        Arc::new(LegacyOwners(self.clone()))
    }

    /// The newest-token book (shared with the identity service).
    #[must_use]
    pub fn tokens(&self) -> tokens::Book {
        self.0.tokens.clone()
    }

    /// The window's counters.
    #[must_use]
    pub fn metrics(&self) -> &Metrics {
        &self.0.metrics
    }

    fn owners_map(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Entry>> {
        self.0.owners.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether pigeon last read `conversation` (of `person`) as legacy; `None` when it has not read it.
    #[must_use]
    pub fn is_legacy(&self, person: &str, conversation: &Uuid) -> Option<bool> {
        self.owners_map()
            .get(conversation)
            .filter(|e| e.person == person)
            .map(|e| e.legacy)
    }

    fn is_deleted(&self, conversation: &Uuid) -> bool {
        self.owners_map().get(conversation).is_some_and(|e| e.deleted)
    }

    /// The window's state for `person`, read from magpie at most every 2 s.
    ///
    /// # Errors
    /// magpie's.
    pub async fn window(&self, person: &str) -> Result<WindowState, magpie_api::client::ClientError> {
        {
            let w = self.0.windows.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((at, s)) = w.get(person)
                && at.elapsed() < self.0.window_ttl
            {
                return Ok(s.clone());
            }
        }
        let Some(app) = self.app() else {
            return Ok(WindowState::closed());
        };
        let s = app.0.magpie.person(person).legacy_window().await?;
        let mut w = self.0.windows.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if w.len() > 20_000 {
            let ttl = self.0.window_ttl;
            w.retain(|_, (at, _)| at.elapsed() < ttl);
        }
        w.insert(person.to_owned(), (Instant::now(), s.clone()));
        Ok(s)
    }

    /// What magpie said about a conversation of `person` (a list entry, a read, a change). A change of owner from
    /// what pigeon read before is a release (legacy → owned) or a reverse hand-over (owned → legacy).
    pub fn observe(&self, person: &str, c: &Conversation) {
        self.set_owner(person, c.id, c.migrating, Some(c.deleted_at.is_some()));
    }

    /// [`Legacy::observe`] for each conversation of a list answer.
    pub fn observe_all(&self, person: &str, list: &[Conversation]) {
        for c in list {
            self.observe(person, c);
        }
    }

    /// The conversation was removed for good: forget it.
    pub fn forget(&self, conversation: &Uuid) {
        self.owners_map().remove(conversation);
    }

    /// Records the owner (a ticket's `native` answer, the 409 re-route, a create through LU), with the same
    /// owner-change handling as [`Legacy::observe`].
    pub fn mark(&self, person: &str, conversation: Uuid, legacy: bool) {
        self.set_owner(person, conversation, legacy, None);
    }

    fn set_owner(&self, person: &str, conversation: Uuid, legacy: bool, deleted: Option<bool>) {
        let before = {
            let mut m = self.owners_map();
            if m.len() > 200_000 {
                // a replica's memory bound; a conversation read again is learnt again
                m.retain(|_, e| e.legacy);
            }
            let prev = m.get(&conversation).cloned();
            let entry = Entry {
                person: person.to_owned(),
                legacy,
                deleted: deleted.unwrap_or_else(|| prev.as_ref().is_some_and(|p| p.deleted)),
            };
            m.insert(conversation, entry);
            prev.filter(|p| p.person == person).map(|p| p.legacy)
        };
        match (before, legacy) {
            (Some(true), false) => self.released(person, conversation),
            (Some(false), true) => self.reversed(person, conversation),
            _ => {}
        }
    }

    /// Learns the owner of a conversation pigeon has not read, before a socket request is routed: only while the
    /// person has legacy conversations (otherwise every unread conversation is owned).
    pub async fn resolve(&self, conn: &Arc<Conn>, conversation: Uuid) {
        if self.is_legacy(&conn.person, &conversation).is_some() {
            return;
        }
        let Some(app) = self.app().cloned() else { return };
        match self.window(&conn.person).await {
            Ok(w) if w.person_legacy > 0 => {}
            _ => return,
        }
        if let Ok(c) = conn.calls(&app.0.magpie).get_conversation(conversation).await {
            self.observe(&conn.person, &c);
        }
    }

    /// A release: the LU feeds of the person's sockets drop the conversation, and every socket watching it gets one
    /// replacement in magpie's epoch and the native feed.
    fn released(&self, person: &str, conversation: Uuid) {
        let Some(app) = self.app().cloned() else { return };
        let Some(hub) = app.hub() else { return };
        for conn in hub.conns_of(person) {
            if let Some(lu) = self.lu_of(conn.id) {
                lu.unwatch(conversation);
            }
        }
        let me = self.clone();
        let person = person.to_owned();
        tokio::spawn(async move {
            let q = magpie_api::rows::TimelineQuery {
                tail: true,
                limit: Some(1),
                ..magpie_api::rows::TimelineQuery::default()
            };
            let mut wait = Duration::from_millis(200);
            for _ in 0..8 {
                match app.0.magpie.person(&person).timeline(conversation, &q).await {
                    Ok(page) => {
                        let n = hub.timeline_subscribers(&person, &conversation).len();
                        hub.owner_changed(&person, conversation, &page.epoch);
                        me.0.metrics.replacements.inc_by(n as u64);
                        tracing::info!(conversation = %conversation, sockets = n, "released: native from now");
                        return;
                    }
                    Err(e) if e.api().is_some_and(|r| r.status == 404) => return,
                    Err(e) => tracing::warn!(error = %e, "release: magpie's epoch not read; retrying"),
                }
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(Duration::from_secs(5));
            }
        });
    }

    /// A reverse hand-over (a rollback): every socket watching the conversation gets one replacement in the old
    /// child's epoch and the LU feed.
    fn reversed(&self, person: &str, conversation: Uuid) {
        let Some(app) = self.app().cloned() else { return };
        let Some(hub) = app.hub() else { return };
        let subs = hub.timeline_subscribers(person, &conversation);
        if subs.is_empty() {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            let Some(first) = subs.first() else { return };
            let lu = me.lu_for(first);
            let ask = serde_json::json!({"type": "fetch_agent_timeline_request", "agentId": conversation,
                                          "direction": "tail", "limit": 1, "projection": "projected"});
            let epoch = match lu.call(ask, READ_TIMEOUT).await {
                Ok((_, p)) => p
                    .get("epoch")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                Err(e) => {
                    tracing::warn!(error = ?e, "reverse: the old epoch not read; the tabs refetch on their own");
                    String::new()
                }
            };
            let payload = crate::paseo::pushes::replacement(&conversation, &epoch);
            let rest = crate::paseo::messages::payload_rest(&payload);
            for conn in &subs {
                conn.send_sub("agent.timeline.replacement", &conn.timeline_sub(), &rest);
                me.lu_for(conn).watch(conversation);
            }
            me.0.metrics.replacements.inc_by(subs.len() as u64);
            tracing::info!(conversation = %conversation, sockets = subs.len(), "reversed: served through LU");
        });
    }

    fn lus(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Arc<lu::Lu>>> {
        self.0.lus.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The LU of a socket, if it has one.
    #[must_use]
    pub fn lu_of(&self, conn: Uuid) -> Option<Arc<lu::Lu>> {
        self.lus().get(&conn).cloned()
    }

    /// The LU of `conn`, opened if it has none. One that dropped while the tab stays is reopened only with a token
    /// that lives [`REOPEN_MIN_LIFE`] more; otherwise the tab is closed 1012 once request-free and reconnects
    /// through the edge (the dropped LU is returned, and its calls fail).
    pub fn lu_for(&self, conn: &Arc<Conn>) -> Arc<lu::Lu> {
        let mut lus = self.lus();
        if let Some(l) = lus.get(&conn.id) {
            if !l.is_dead() {
                return l.clone();
            }
            let holder = conn.viewer.as_ref().map_or(conn.person.as_str(), |v| v.admin.as_str());
            let lives = self.0.tokens.newest(holder).is_some_and(|(_, exp)| {
                exp - crate::identity::now_secs() >= i64::try_from(REOPEN_MIN_LIFE.as_secs()).unwrap_or(i64::MAX)
            });
            if !lives {
                let l = l.clone();
                drop(lus);
                tracing::info!(conn = %conn.id, "LU dropped and the token is near its end: the tab reconnects");
                let c = conn.clone();
                tokio::spawn(async move {
                    while !c.request_free() && !c.is_closing() {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    c.close_after_queued(1012, "reconnect");
                });
                return l;
            }
        }
        let watched: std::collections::HashSet<Uuid> = conn
            .timeline_agents()
            .into_iter()
            .filter(|c| self.is_legacy(&conn.person, c) == Some(true))
            .collect();
        let l = lu::Lu::open(self.clone(), conn, watched);
        lus.insert(conn.id, l.clone());
        l
    }

    /// An LU ended: it is forgotten once its socket is gone (a dropped LU of a live socket stays, for the reopen
    /// rule of [`Legacy::lu_for`]).
    pub(crate) fn lu_ended(&self, conn: Uuid, lu: &Arc<lu::Lu>, conn_gone: bool) {
        if conn_gone {
            let mut lus = self.lus();
            if lus.get(&conn).is_some_and(|l| Arc::ptr_eq(l, lu)) {
                lus.remove(&conn);
            }
        }
    }

    /// LU sockets open (for tests and `watch`).
    #[must_use]
    pub fn lu_open(&self) -> usize {
        self.lus().values().filter(|l| !l.is_dead()).count()
    }
}
