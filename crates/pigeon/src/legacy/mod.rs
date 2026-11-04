//! The switch window (DESIGN 9.2-9.5; switch-final 4.1, 3.2 C4-C5, 11, 23 C17): pigeon fronts the conversations
//! the old stack still owns, with nothing read-only, cut or lost. Deleted in A6 with the window's magpie routes
//! (DESIGN 9.7), except the request-free-first drain, which lives in `feeds` and stays.
//!
//! - **Ownership.** magpie's conversation row says who owns a conversation (`migrating`: the old stack). pigeon keeps
//!   what it last read per conversation ([`Legacy::observe`], fed by every list answer, conversation read and person
//!   change, ordered by when each was asked, an owner change confirmed by a fresh read), and asks magpie for a
//!   conversation it has not seen while the person has legacy conversations. A
//!   native write that magpie answers 409 `conversation_migrating` with `route: "legacy"` is sent again here (the
//!   re-route). The installed [`feeds::Owners`] hands a legacy conversation's socket requests and timeline
//!   subscriptions to the legacy delegate.
//! - **The legacy upstream (LU).** One WebSocket per native socket to the old router's `/paseo-ws` through the
//!   bridge Service [`LU_BASE`] (DESIGN 15.5 B1), opened the first time that socket touches a legacy conversation, as
//!   the person with the newest Access token pigeon verified for them ([`tokens::Book`]), with pigeon's pair token in
//!   [`LU_HEADER`] (the old router counts the socket as LU only then and strips the header, switch-final C5). A view's
//!   socket carries the same view claims signed with the old router's key ([`OLD_VIEW_AS_HEADER`]). It carries the
//!   conversation's timeline, live stream and writes ([`lu`]); a write goes under a magpie ticket that closes only on
//!   the old stack's outcome ([`delegate`]). An LU that drops while its tab stays is reopened at once (with one
//!   replacement per watched conversation in the old epoch) only with a token that lives at least [`REOPEN_MIN_LIFE`]
//!   more and after it had been up [`REOPEN_MIN_UP`]; otherwise the tab is closed 1012 at a request-free moment and
//!   reconnects through the edge, which checks the Access session itself ([`Legacy::lu_ended`]; DESIGN 9.5's
//!   deviation E4). An LU answered 401, or one whose holder has no unexpired token here, gives the tab pigeon's
//!   signed-out answer (4401 on the socket, 401 `NOT_SIGNED_IN` on HTTP), never a write error
//!   (`pigeon_lu_401_total`). An LU that streams nothing and carries no call closes after
//!   [`crate::Tuning::legacy_lu_idle`] while its tab stays (the old stack's LU count reaches zero, A5); the tab's next
//!   legacy request opens a new one like a first one.
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
pub use delegate::is_write;
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
/// The old child's busy state (`{running, processes, preparing, pending}`, the old `jarvis-switch.routes.ts`).
pub const OLD_BUSY: &str = "/api/jarvis/switch/busy";
/// The old warm route.
pub const OLD_WARM: &str = "/api/jarvis/warm";
/// The old route's per-file cap (the old `assets.routes.ts`): every upload keeps it while any conversation is
/// legacy, so a file either stack accepted is acceptable to both (DESIGN 9.5).
pub const OLD_FILE_MAX: u64 = 30 * 1024 * 1024;
/// The old timeline's id of a user message (`paseo-hub.ts`: `paseo-user-<clientMessageId>`).
pub const OLD_USER_ROW_PREFIX: &str = "paseo-user-";
/// An LU that dropped is reopened on the same tab only with a token that lives this much longer (a reopen with no
/// reconnect, so a blip of the cross-cluster bridge does not reconnect every tab on it: DESIGN 9.5's deviation E4).
pub const REOPEN_MIN_LIFE: Duration = Duration::from_hours(1);
/// The LU upgrade and its hello (a cold old child starts within it).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// A read through LU.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// An LU connection (socket and HTTP) is aborted once bytes it sent stay unacknowledged this long (`TCP_USER_TIMEOUT`):
/// a write past its bound is not retransmitted for long after a partition of the bridge heals.
pub const LU_TCP_USER_TIMEOUT: Duration = Duration::from_secs(5);
/// A send's claim must be answered within this after pigeon sent it for the write to start: magpie stamps the claim
/// when it takes it, and its claimed write settles [`magpie_api::legacy::TICKET_CLAIM_SETTLE`] later, so a write
/// starting later than this could still land after that (and after the ticket's hold).
pub const CLAIM_ANSWER_MAX: Duration = Duration::from_secs(3);

// a claimed write starts within the claim answer's bound, is on the wire within the write timeout, and is delivered
// within the TCP user timeout or never: all inside the claim's settle time
const _: () = assert!(
    CLAIM_ANSWER_MAX.as_secs() + magpie_api::legacy::TICKET_WRITE_TIMEOUT.as_secs() + LU_TCP_USER_TIMEOUT.as_secs()
        <= magpie_api::legacy::TICKET_CLAIM_SETTLE.as_secs()
);

/// An LU that lived less than this before it dropped is not reopened on its own: the tab is closed 1012 at a
/// request-free moment and reconnects (an old router that keeps refusing is not hammered).
pub const REOPEN_MIN_UP: Duration = Duration::from_secs(5);

/// Who serves a conversation, as pigeon last read it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    person: String,
    legacy: bool,
    deleted: bool,
    /// As of when: the start of the read that said so, or when a change or a ticket answer said so. An observation
    /// that started earlier is older than what is held and changes nothing.
    at: Instant,
}

/// Where an owner observation comes from.
#[derive(Debug, Clone, Copy)]
pub enum Seen {
    /// A list answer or a conversation read that pigeon sent at this instant.
    Read(Instant),
    /// A change-feed upsert (in order, but it can lag a ticket answer).
    Change,
    /// A ticket's `native`, the 409 re-route, a create through LU: what magpie answered just now about a write.
    Mark,
    /// The fresh read that confirms an owner change, sent at this instant.
    Confirmed(Instant),
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
    /// `pigeon_lu_reopened_total`: LUs reopened on their own after they dropped while their tab stayed.
    pub lu_reopened: Counter,
    /// `pigeon_legacy_release_unread_total`: releases whose magpie epoch could not be read (the watching sockets
    /// were closed 1012 to resubscribe instead).
    pub release_unread: Counter,
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
            lu_reopened: Counter::default(),
            release_unread: Counter::default(),
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
        r.register(
            "lu_reopened",
            "Legacy upstream sockets reopened after a drop while the tab stayed",
            m.lu_reopened.clone(),
        );
        r.register(
            "legacy_release_unread",
            "Releases whose new epoch could not be read: the watching tabs were closed to resubscribe (zero)",
            m.release_unread.clone(),
        );
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
    /// Owner changes being confirmed by a fresh read; `true`: another observation came meanwhile (read again).
    confirming: Mutex<HashMap<Uuid, bool>>,
    windows: Mutex<HashMap<String, (Instant, WindowState)>>,
    window_ttl: Duration,
    pub(crate) heartbeat: Duration,
    pub(crate) transit: Duration,
    pub(crate) write_timeout: Duration,
    pub(crate) lu_idle: Duration,
    release_wait: Duration,
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
        let http = aviary_core::http::client_builder(build, None).connect_timeout(Duration::from_secs(5));
        // a write's bytes are delivered within their bound plus this, or never (an aborted request's buffered body is
        // not retransmitted for long after a partition heals; DESIGN 9.5)
        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        let http = http.tcp_user_timeout(LU_TCP_USER_TIMEOUT);
        let http = http.build().unwrap_or_default();
        let inner = Arc::new(Inner {
            base: upstreams.legacy.trim_end_matches('/').to_owned(),
            pair: secrets.token_ui.clone(),
            user_agent: format!("pigeon/{}", aviary_core::version::sha12()),
            http,
            tokens: tokens::Book::new(secrets.legacy_view_as_key.clone()),
            owners: Mutex::new(HashMap::new()),
            confirming: Mutex::new(HashMap::new()),
            windows: Mutex::new(HashMap::new()),
            window_ttl: tuning.legacy_window,
            heartbeat: tuning.legacy_heartbeat,
            transit: tuning.legacy_transit,
            write_timeout: tuning
                .legacy_write_timeout
                .min(magpie_api::legacy::TICKET_WRITE_TIMEOUT),
            lu_idle: tuning.legacy_lu_idle,
            release_wait: tuning.legacy_release_wait,
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

    /// What magpie said about a conversation of `person` (a list entry or a read sent at `Seen::Read`, a change). A
    /// change of owner from what pigeon holds is a release (legacy → owned) or a reverse hand-over (owned → legacy),
    /// acted on once a fresh read confirms it: a list or read answered late, or a change that lags a ticket's answer,
    /// can say what is no longer so, and must not flip the owner back (a spurious replacement and LU watch).
    pub fn observe(&self, person: &str, c: &Conversation, seen: Seen) {
        self.set_owner(person, c.id, c.migrating, Some(c.deleted_at.is_some()), seen);
    }

    /// [`Legacy::observe`] for each conversation of a list answer.
    pub fn observe_all(&self, person: &str, list: &[Conversation], seen: Seen) {
        for c in list {
            self.observe(person, c, seen);
        }
    }

    /// The conversation was removed for good: forget it.
    pub fn forget(&self, conversation: &Uuid) {
        self.owners_map().remove(conversation);
    }

    /// Records the owner (a ticket's `native` answer, the 409 re-route, a create through LU), with the same
    /// owner-change handling as [`Legacy::observe`] (no confirming read: magpie just said so about a write).
    pub fn mark(&self, person: &str, conversation: Uuid, legacy: bool) {
        self.set_owner(person, conversation, legacy, None, Seen::Mark);
    }

    /// A ticketed write's outcome as pigeon's view of Recently deleted (magpie's `deletedAt` of a legacy
    /// conversation lags the old child until the importer syncs it).
    pub(crate) fn set_deleted(&self, conversation: &Uuid, deleted: bool) {
        if let Some(e) = self.owners_map().get_mut(conversation) {
            e.deleted = deleted;
        }
    }

    fn set_owner(&self, person: &str, conversation: Uuid, legacy: bool, deleted: Option<bool>, seen: Seen) {
        let now = Instant::now();
        let (since, confirm) = match seen {
            Seen::Read(at) => (at, true),
            Seen::Change => (now, true),
            Seen::Mark => (now, false),
            Seen::Confirmed(at) => (at, false),
        };
        let before = {
            let mut m = self.owners_map();
            if m.len() > 200_000 {
                // a replica's memory bound; a conversation read again is learnt again
                m.retain(|_, e| e.legacy);
            }
            let prev = m.get(&conversation).cloned().filter(|p| p.person == person);
            if let Some(p) = &prev {
                if since < p.at {
                    // older than what is held
                    return;
                }
                if p.legacy != legacy && confirm {
                    drop(m);
                    self.confirm(person, conversation);
                    return;
                }
            }
            let entry = Entry {
                person: person.to_owned(),
                legacy,
                deleted: deleted.unwrap_or_else(|| prev.as_ref().is_some_and(|p| p.deleted)),
                at: since,
            };
            m.insert(conversation, entry);
            prev.map(|p| p.legacy)
        };
        match (before, legacy) {
            (Some(true), false) => self.released(person, conversation),
            (Some(false), true) => self.reversed(person, conversation),
            _ => {}
        }
    }

    /// An observation says the owner changed: a read sent now says whether it did (one at a time per
    /// conversation; an observation arriving meanwhile reads again after it).
    fn confirm(&self, person: &str, conversation: Uuid) {
        {
            let mut c = self
                .0
                .confirming
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(again) = c.get_mut(&conversation) {
                *again = true;
                return;
            }
            c.insert(conversation, false);
        }
        let Some(app) = self.app().cloned() else {
            self.0
                .confirming
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&conversation);
            return;
        };
        let me = self.clone();
        let person = person.to_owned();
        tokio::spawn(async move {
            loop {
                let mut wait = Duration::from_millis(200);
                for _ in 0..5 {
                    let sent = Instant::now();
                    match app.0.magpie.person(&person).get_conversation(conversation).await {
                        Ok(c) => {
                            me.observe(&person, &c, Seen::Confirmed(sent));
                            break;
                        }
                        Err(e) if e.api().is_some_and(|r| r.status == 404) => break,
                        Err(e) => tracing::warn!(error = %e, "owner change not confirmed; retrying"),
                    }
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(Duration::from_secs(2));
                }
                let mut c =
                    me.0.confirming
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                if c.get(&conversation) == Some(&true) {
                    c.insert(conversation, false);
                } else {
                    c.remove(&conversation);
                    return;
                }
            }
        });
    }

    /// Learns the owner of a conversation pigeon has not read, before a socket request is routed: only while the
    /// person has legacy conversations (otherwise every unread conversation is owned). `Err` when it cannot be told
    /// (the window or the conversation not read): a read then answers a retryable error rather than magpie's copy
    /// of what may be a legacy conversation; a write goes natively, where magpie's 409 re-routes it.
    ///
    /// # Errors
    /// The owner could not be read.
    pub async fn resolve(&self, conn: &Arc<Conn>, conversation: Uuid) -> Result<(), Unknown> {
        if self.is_legacy(&conn.person, &conversation).is_some() {
            return Ok(());
        }
        let Some(app) = self.app().cloned() else { return Ok(()) };
        match self.window(&conn.person).await {
            Ok(w) if w.person_legacy > 0 => {}
            Ok(_) => return Ok(()),
            Err(_) => return Err(Unknown),
        }
        let sent = Instant::now();
        match conn.calls(&app.0.magpie).get_conversation(conversation).await {
            Ok(c) => {
                self.observe(&conn.person, &c, Seen::Read(sent));
                Ok(())
            }
            // not the person's (or gone): magpie's own answer is the right one
            Err(e) if e.api().is_some_and(|r| r.status < 500 && r.status != 429) => Ok(()),
            Err(_) => Err(Unknown),
        }
    }

    /// A release: every socket watching the conversation gets one replacement in magpie's epoch and the native feed;
    /// the LU feeds of the person's sockets drop the conversation once that is done (their frames of it already stop
    /// at the tab, [`lu`]'s owner check). magpie's epoch is read until it answers while a socket watches; if it never
    /// does within [`crate::Tuning::legacy_release_wait`], the watching sockets are closed 1012 when request-free and resubscribe.
    fn released(&self, person: &str, conversation: Uuid) {
        let Some(app) = self.app().cloned() else { return };
        let Some(hub) = app.hub() else { return };
        let me = self.clone();
        let person = person.to_owned();
        tokio::spawn(async move {
            let q = magpie_api::rows::TimelineQuery {
                tail: true,
                limit: Some(1),
                ..magpie_api::rows::TimelineQuery::default()
            };
            let unwatch = |me: &Legacy| {
                for conn in hub.conns_of(&person) {
                    if let Some(lu) = me.lu_of(conn.id) {
                        lu.unwatch(conversation);
                    }
                }
            };
            let started = Instant::now();
            let mut wait = Duration::from_millis(200);
            loop {
                if hub.timeline_subscribers(&person, &conversation).is_empty() {
                    unwatch(&me);
                    return;
                }
                match app.0.magpie.person(&person).timeline(conversation, &q).await {
                    Ok(page) => {
                        let n = hub.timeline_subscribers(&person, &conversation).len();
                        hub.owner_changed(&person, conversation, &page.epoch);
                        unwatch(&me);
                        me.0.metrics.replacements.inc_by(n as u64);
                        tracing::info!(conversation = %conversation, sockets = n, "released: native from now");
                        return;
                    }
                    Err(e) if e.api().is_some_and(|r| r.status == 404) => {
                        unwatch(&me);
                        return;
                    }
                    Err(e) => tracing::warn!(error = %e, "release: magpie's epoch not read; retrying"),
                }
                if started.elapsed() >= me.0.release_wait {
                    break;
                }
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(Duration::from_secs(5));
            }
            let subs = hub.timeline_subscribers(&person, &conversation);
            me.0.metrics.release_unread.inc();
            tracing::warn!(conversation = %conversation, sockets = subs.len(),
                           "release: magpie's epoch never read; the watching tabs reconnect");
            unwatch(&me);
            for c in subs {
                close_when_request_free(c);
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
            let epoch = me.old_epoch(&lu, conversation).await;
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

    /// Whether a dropped LU of `conn` may be reopened: its token holder's newest token lives [`REOPEN_MIN_LIFE`]
    /// more.
    fn may_reopen(&self, conn: &Conn) -> bool {
        let holder = conn.viewer.as_ref().map_or(conn.person.as_str(), |v| v.admin.as_str());
        self.0.tokens.newest(holder).is_some_and(|(_, exp)| {
            exp - crate::identity::now_secs() >= i64::try_from(REOPEN_MIN_LIFE.as_secs()).unwrap_or(i64::MAX)
        })
    }

    /// The LU of `conn`, opened if it has none. One that dropped while the tab stays is reopened only with a token
    /// that lives [`REOPEN_MIN_LIFE`] more; otherwise the tab is closed 1012 once request-free and reconnects
    /// through the edge (the dropped LU is returned, and its calls fail).
    pub fn lu_for(&self, conn: &Arc<Conn>) -> Arc<lu::Lu> {
        let mut lus = self.lus();
        if let Some(l) = lus.get(&conn.id) {
            if !l.is_dead() {
                l.touch();
                return l.clone();
            }
            // one closed as unused is opened again like a first one; one that dropped follows the reopen rule
            if !l.closed_idle() && !self.may_reopen(conn) {
                let l = l.clone();
                drop(lus);
                tracing::info!(conn = %conn.id, "LU dropped and the token is near its end: the tab reconnects");
                close_when_request_free(conn.clone());
                return l;
            }
        }
        let watched: std::collections::HashSet<Uuid> = conn
            .timeline_agents()
            .into_iter()
            .filter(|c| self.is_legacy(&conn.person, c) == Some(true))
            .collect();
        let l = lu::Lu::open(self.clone(), conn, watched);
        // a dropped LU of a tab that has gone since is forgotten
        lus.retain(|_, l| !l.is_dead() || l.conn_alive());
        lus.insert(conn.id, l.clone());
        l
    }

    /// An LU ended. Once its socket is gone it is forgotten. If the tab stays (an old ui pod restart, a bridge blip,
    /// an old router error), the tab must not be left with a silent stream: an LU answered 401 gives it the
    /// signed-out answer (4401); one that streamed legacy conversations is reopened at once when the token rule allows
    /// and it had been up [`REOPEN_MIN_UP`], and each watched conversation gets one replacement in the old child's
    /// epoch (the tab refetches across the gap); otherwise the tab is closed 1012 once request-free and resumes. An LU
    /// that watched nothing is reopened by the tab's next request ([`Legacy::lu_for`]).
    pub(crate) fn lu_ended(&self, conn: Option<Arc<Conn>>, lu: &Arc<lu::Lu>) {
        let Some(conn) = conn.filter(|c| !c.is_closing()) else {
            let mut lus = self.lus();
            if lus.get(&lu.conn_id()).is_some_and(|l| Arc::ptr_eq(l, lu)) {
                lus.remove(&lu.conn_id());
            }
            return;
        };
        if lu.ended_with() == Some(lu::LuError::SignedOut) {
            conn.close(4401, "signed out");
            return;
        }
        if lu.closed_idle() {
            // it streamed nothing: the tab's next legacy request opens a new one
            return;
        }
        let watched: Vec<Uuid> = lu
            .watched()
            .into_iter()
            .filter(|c| conn.watches(c) && self.is_legacy(&conn.person, c) == Some(true))
            .collect();
        if watched.is_empty() {
            return;
        }
        let up = lu.up_for();
        let me = self.clone();
        tokio::spawn(async move {
            // past a blip
            tokio::time::sleep(Duration::from_millis(300)).await;
            if conn.is_closing() {
                return;
            }
            if up.is_none_or(|d| d < REOPEN_MIN_UP) || !me.may_reopen(&conn) {
                tracing::info!(conn = %conn.id, "LU dropped and not reopened: the tab reconnects");
                close_when_request_free(conn);
                return;
            }
            let lu = me.lu_for(&conn);
            if lu.ready().await.is_err() {
                tracing::info!(conn = %conn.id, "LU dropped and did not reopen: the tab reconnects");
                close_when_request_free(conn);
                return;
            }
            me.0.metrics.lu_reopened.inc();
            for c in watched {
                if !conn.watches(&c) || me.is_legacy(&conn.person, &c) != Some(true) {
                    continue;
                }
                let epoch = me.old_epoch(&lu, c).await;
                let payload = crate::paseo::pushes::replacement(&c, &epoch);
                let rest = crate::paseo::messages::payload_rest(&payload);
                conn.send_sub("agent.timeline.replacement", &conn.timeline_sub(), &rest);
                me.0.metrics.replacements.inc();
            }
            tracing::info!(conn = %conn.id, "LU reopened after a drop");
        });
    }

    /// The old child's epoch of `conversation` (empty when it cannot be read: the tab refetches either way).
    async fn old_epoch(&self, lu: &lu::Lu, conversation: Uuid) -> String {
        let ask = serde_json::json!({"type": "fetch_agent_timeline_request", "agentId": conversation,
                                      "direction": "tail", "limit": 1, "projection": "projected"});
        match lu.call(ask, READ_TIMEOUT).await {
            Ok((_, p)) => p
                .get("epoch")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned(),
            Err(e) => {
                tracing::warn!(error = ?e, "the old epoch not read; the tabs refetch on their own");
                String::new()
            }
        }
    }

    /// LU sockets open (for tests and `watch`).
    #[must_use]
    pub fn lu_open(&self) -> usize {
        self.lus().values().filter(|l| !l.is_dead()).count()
    }
}

/// The owner of a conversation could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unknown;

/// Closes `conn` 1012 at its first request-free moment (it reconnects and resumes).
pub(crate) fn close_when_request_free(c: Arc<Conn>) {
    tokio::spawn(async move {
        while !c.request_free() && !c.is_closing() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        c.close_after_queued(1012, "reconnect");
    });
}
