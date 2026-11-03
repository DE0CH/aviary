//! Live streaming from magpie to the open sockets of this replica (spec 8): the [`Hub`] of sockets, their
//! subscriptions, and the two kinds of long-poll pollers that feed them ([`change_poller`] per person,
//! [`timeline_poller`] per person and conversation).
//!
//! Everything here is memory of one replica, rebuilt from magpie on a reconnect (spec 10): sockets, the index of who
//! watches which conversation's timeline, the pollers, and a 2 s single-flight cache of list snapshots.
//!
//! **Seams for the switch window** (WP-50): [`Owners`] says, per conversation, whether this pigeon serves it from
//! magpie (the default, [`NativeOnly`]) or hands its requests and timeline subscriptions to a [`Delegate`];
//! [`Hub::timeline_subscribers`] lists the sockets watching a conversation (per-socket subscription tracking);
//! [`Hub::owner_changed`] moves a conversation's watchers to the native feed with one `agent.timeline.replacement`;
//! and [`Hub::drain`] closes request-free sockets first.

pub mod change_poller;
pub mod timeline_poller;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use magpie_api::conversations::{ListQuery, ListResponse};
use serde_json::Value;
use tokio::sync::{OnceCell, mpsc};
use uuid::Uuid;

use crate::App;
use crate::paseo::dispatch::{Fail, fail};
use crate::paseo::transport::{Conn, ListKind};

/// Sockets per replica (spec 13): an upgrade beyond is answered 503.
pub const MAX_SOCKETS: usize = 10_000;
/// Sockets per person per replica: the oldest is closed 1008 beyond.
pub const MAX_PERSON_SOCKETS: usize = 64;
/// Pollers per replica: beyond, new subscriptions get no live rows (the app's gap checks still fetch them).
pub const MAX_POLLERS: usize = 4_000;
/// The list micro-cache (7.5.2).
pub const LIST_CACHE: Duration = Duration::from_secs(2);

/// A future the seams return.
pub type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Who serves a conversation's requests on a native socket (WP-50's per-conversation owner switch).
pub trait Owners: Send + Sync {
    /// `None`: this pigeon, from magpie. `Some`: the delegate serves the conversation's requests and timeline.
    fn delegate(&self, person: &str, conversation: &Uuid) -> Option<Arc<dyn Delegate>>;
}

/// Serves the conversations [`Owners`] hands it, on the native socket.
pub trait Delegate: Send + Sync {
    /// A conversation-scoped request (`kind`, the session message as received); the delegate answers on `socket`.
    fn request(&self, socket: Arc<Conn>, kind: String, message: Value) -> BoxFuture;
    /// `socket` now watches the conversation's timeline.
    fn subscribe(&self, socket: Arc<Conn>, conversation: Uuid);
    /// …and no longer does.
    fn unsubscribe(&self, socket: &Arc<Conn>, conversation: Uuid);
}

/// Every conversation is served from magpie (outside the switch window, always).
#[derive(Debug, Clone, Copy, Default)]
pub struct NativeOnly;

impl Owners for NativeOnly {
    fn delegate(&self, _person: &str, _conversation: &Uuid) -> Option<Arc<dyn Delegate>> {
        None
    }
}

/// (person, viewer (admin, view session) of a view-as socket, cursor, limit).
type ListKey = (String, Option<(String, String)>, Option<String>, u32);
type ListCell = Arc<OnceCell<Result<ListResponse, Fail>>>;

#[derive(Default)]
struct State {
    conns: HashMap<Uuid, Arc<Conn>>,
    by_person: HashMap<String, Vec<Uuid>>,
    /// (person, conversation) → sockets watching its timeline.
    timeline: HashMap<(String, Uuid), HashSet<Uuid>>,
    change_pollers: HashMap<String, mpsc::UnboundedSender<change_poller::Ctl>>,
    timeline_pollers: HashMap<(String, Uuid), mpsc::UnboundedSender<timeline_poller::Ctl>>,
    lists: HashMap<ListKey, (Instant, ListCell)>,
    /// SIGTERM: new upgrades get 503; the pollers keep feeding the sockets still open.
    draining: bool,
    /// The drain ended: no socket is left and no poller starts again.
    drained: bool,
}

/// The sockets and pollers of this replica. Cheap to clone.
#[derive(Clone)]
pub struct Hub(Arc<HubInner>);

struct HubInner {
    st: Mutex<State>,
    app: OnceLock<App>,
    owners: Arc<dyn Owners>,
    large: Arc<tokio::sync::Semaphore>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub").finish_non_exhaustive()
    }
}

impl Default for Hub {
    fn default() -> Self {
        Self::new(Arc::new(NativeOnly))
    }
}

/// Large inbound messages (over 8 MiB) handled at once per replica (7.5.4).
pub const LARGE_AT_ONCE: usize = 3;
/// What a large inbound message is.
pub const LARGE_MESSAGE: usize = 8 << 20;

impl Hub {
    /// A hub whose conversations `owners` routes.
    #[must_use]
    pub fn new(owners: Arc<dyn Owners>) -> Self {
        Self(Arc::new(HubInner {
            st: Mutex::new(State::default()),
            app: OnceLock::new(),
            owners,
            large: Arc::new(tokio::sync::Semaphore::new(LARGE_AT_ONCE)),
        }))
    }

    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.st.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn set_app(&self, app: &App) {
        let _ = self.0.app.set(app.clone());
    }

    pub(crate) fn app(&self) -> Option<&App> {
        self.0.app.get()
    }

    /// The semaphore large inbound messages wait for.
    pub(crate) fn large(&self) -> Arc<tokio::sync::Semaphore> {
        self.0.large.clone()
    }

    /// The delegate of a conversation, if [`Owners`] names one.
    #[must_use]
    pub fn delegate(&self, person: &str, conversation: &Uuid) -> Option<Arc<dyn Delegate>> {
        self.0.owners.delegate(person, conversation)
    }

    /// Whether the drain began (new upgrades get 503).
    #[must_use]
    pub fn draining(&self) -> bool {
        self.st().draining
    }

    /// Open sockets.
    #[must_use]
    pub fn sockets(&self) -> usize {
        self.st().conns.len()
    }

    /// Running pollers (change, timeline).
    #[must_use]
    pub fn pollers(&self) -> (usize, usize) {
        let st = self.st();
        (st.change_pollers.len(), st.timeline_pollers.len())
    }

    /// Whether a new socket would exceed [`MAX_SOCKETS`].
    #[must_use]
    pub fn full(&self) -> bool {
        self.st().conns.len() >= MAX_SOCKETS
    }

    /// Adds a socket; the person's change poller starts if it is not running. Beyond [`MAX_PERSON_SOCKETS`] the
    /// person's oldest socket is closed 1008. A socket whose upgrade was in flight when the drain began (after the
    /// drain listed the sockets to close) is closed 1012 at once: it has nothing in flight yet.
    pub(crate) fn register(&self, conn: &Arc<Conn>) {
        let (oldest, draining) = {
            let mut st = self.st();
            let draining = st.draining;
            st.conns.insert(conn.id, conn.clone());
            let list = st.by_person.entry(conn.person.clone()).or_default();
            list.push(conn.id);
            let oldest = if list.len() > MAX_PERSON_SOCKETS {
                let id = list.remove(0);
                st.conns.get(&id).cloned()
            } else {
                None
            };
            (oldest, draining)
        };
        if let Some(old) = oldest {
            old.close(1008, "too many connections");
        }
        if draining {
            conn.close_after_queued(1012, "service restart");
            return;
        }
        self.ensure_change_poller(&conn.person);
    }

    /// Removes a closed socket and its subscriptions.
    pub(crate) fn unregister(&self, conn: &Arc<Conn>) {
        let watched: Vec<Uuid> = conn.timeline_agents().into_iter().collect();
        {
            let mut st = self.st();
            st.conns.remove(&conn.id);
            if let Some(list) = st.by_person.get_mut(&conn.person) {
                list.retain(|id| *id != conn.id);
                if list.is_empty() {
                    st.by_person.remove(&conn.person);
                }
            }
            for c in &watched {
                let key = (conn.person.clone(), *c);
                if let Some(set) = st.timeline.get_mut(&key) {
                    set.remove(&conn.id);
                    if set.is_empty() {
                        st.timeline.remove(&key);
                    }
                }
            }
        }
        for c in watched {
            if let Some(d) = self.delegate(&conn.person, &c) {
                d.unsubscribe(conn, c);
            }
        }
    }

    /// The person's open sockets on this replica.
    #[must_use]
    pub fn conns_of(&self, person: &str) -> Vec<Arc<Conn>> {
        let st = self.st();
        st.by_person
            .get(person)
            .map(|ids| ids.iter().filter_map(|id| st.conns.get(id).cloned()).collect())
            .unwrap_or_default()
    }

    /// The sockets of `person` watching `conversation`'s timeline (per-socket subscription tracking).
    #[must_use]
    pub fn timeline_subscribers(&self, person: &str, conversation: &Uuid) -> Vec<Arc<Conn>> {
        let st = self.st();
        st.timeline
            .get(&(person.to_owned(), *conversation))
            .map(|ids| ids.iter().filter_map(|id| st.conns.get(id).cloned()).collect())
            .unwrap_or_default()
    }

    /// Replaces the conversations `conn` watches (7.5.3): the index follows, native pollers start for the new ones
    /// (delegated ones go to their delegate), and pollers nobody watches any more linger and stop (8.2).
    pub(crate) fn set_timeline(&self, conn: &Arc<Conn>, agents: &HashSet<Uuid>) {
        let before = conn.replace_timeline_agents(agents.clone());
        let added: Vec<Uuid> = agents.difference(&before).copied().collect();
        let removed: Vec<Uuid> = before.difference(agents).copied().collect();
        {
            let mut st = self.st();
            for c in &added {
                st.timeline
                    .entry((conn.person.clone(), *c))
                    .or_default()
                    .insert(conn.id);
            }
            for c in &removed {
                let key = (conn.person.clone(), *c);
                if let Some(set) = st.timeline.get_mut(&key) {
                    set.remove(&conn.id);
                    if set.is_empty() {
                        st.timeline.remove(&key);
                    }
                }
            }
        }
        for c in removed {
            if let Some(d) = self.delegate(&conn.person, &c) {
                d.unsubscribe(conn, c);
            }
        }
        for c in added {
            match self.delegate(&conn.person, &c) {
                Some(d) => d.subscribe(conn.clone(), c),
                None => self.ensure_timeline_poller(&conn.person, c),
            }
        }
    }

    /// A conversation moved to the native feed (WP-50's release): every socket watching it gets one
    /// `agent.timeline.replacement` in magpie's `epoch` and is served by a native poller from now on.
    pub fn owner_changed(&self, person: &str, conversation: Uuid, epoch: &str) {
        let payload = crate::paseo::pushes::replacement(&conversation, epoch);
        let rest = crate::paseo::messages::payload_rest(&payload);
        for conn in self.timeline_subscribers(person, &conversation) {
            conn.send_sub("agent.timeline.replacement", &conn.timeline_sub(), &rest);
        }
        if self.delegate(person, &conversation).is_none() {
            // a native poller still lingering from before the delegation holds an old cursor and epoch: it starts
            // again from the tail, with this replacement counted as sent
            let restart = timeline_poller::Ctl::Restart {
                announced: epoch.to_owned(),
            };
            if !self.to_timeline(person, conversation, restart) {
                self.ensure_timeline_poller(person, conversation);
            }
        }
    }

    /// Hands a list subscription's snapshot head to the person's change poller (8.3).
    pub(crate) fn join_list(&self, conn: &Arc<Conn>, kind: ListKind, head: u64) {
        self.ensure_change_poller(&conn.person);
        let tx = self.st().change_pollers.get(&conn.person).cloned();
        if let Some(tx) = tx {
            let _ = tx.send(change_poller::Ctl::Join {
                conn: conn.clone(),
                kind,
                head,
            });
        }
    }

    /// Hands a change-feed turn or replacement to the conversation's timeline poller, if one runs here (8.4).
    pub(crate) fn to_timeline(&self, person: &str, conversation: Uuid, ctl: timeline_poller::Ctl) -> bool {
        let tx = self
            .st()
            .timeline_pollers
            .get(&(person.to_owned(), conversation))
            .cloned();
        tx.is_some_and(|tx| tx.send(ctl).is_ok())
    }

    fn poller_count(st: &State) -> usize {
        st.change_pollers.len() + st.timeline_pollers.len()
    }

    pub(crate) fn ensure_change_poller(&self, person: &str) {
        let Some(app) = self.app().cloned() else {
            return;
        };
        let mut st = self.st();
        if st.drained {
            return;
        }
        if st.change_pollers.get(person).is_some_and(|tx| !tx.is_closed()) {
            return;
        }
        if Self::poller_count(&st) >= MAX_POLLERS {
            drop(st);
            tracing::warn!("poller limit reached: no change feed for a person");
            app.0.metrics.poller_limit("change");
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        st.change_pollers.insert(person.to_owned(), tx);
        drop(st);
        let gauge = app.0.metrics.poller("change");
        let run = change_poller::run(self.clone(), app, person.to_owned(), rx);
        tokio::spawn(async move {
            let _gauge = gauge;
            run.await;
        });
    }

    pub(crate) fn ensure_timeline_poller(&self, person: &str, conversation: Uuid) {
        let Some(app) = self.app().cloned() else {
            return;
        };
        let key = (person.to_owned(), conversation);
        let mut st = self.st();
        if st.drained {
            return;
        }
        if st.timeline_pollers.get(&key).is_some_and(|tx| !tx.is_closed()) {
            return;
        }
        if Self::poller_count(&st) >= MAX_POLLERS {
            drop(st);
            tracing::warn!("poller limit reached: a timeline is served without live rows");
            app.0.metrics.poller_limit("timeline");
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        st.timeline_pollers.insert(key, tx);
        drop(st);
        let gauge = app.0.metrics.poller("timeline");
        let run = timeline_poller::run(self.clone(), app, person.to_owned(), conversation, rx);
        tokio::spawn(async move {
            let _gauge = gauge;
            run.await;
        });
    }

    /// Ends a change poller when the person has had no socket here for `linger` (or the drain ended); `true` when
    /// it should stop. While the drain runs the poller keeps feeding the sockets still open (11.2: pollers stop
    /// after the sockets).
    pub(crate) fn change_poller_done(&self, person: &str, idle_since: &mut Option<Instant>, linger: Duration) -> bool {
        let mut st = self.st();
        if st.drained {
            return true;
        }
        if st.by_person.contains_key(person) {
            *idle_since = None;
            return false;
        }
        let since = *idle_since.get_or_insert_with(Instant::now);
        if since.elapsed() >= linger {
            st.change_pollers.remove(person);
            return true;
        }
        false
    }

    /// Ends a timeline poller when nobody here has watched its conversation for `linger`.
    pub(crate) fn timeline_poller_done(
        &self,
        person: &str,
        conversation: Uuid,
        idle_since: &mut Option<Instant>,
        linger: Duration,
    ) -> bool {
        let key = (person.to_owned(), conversation);
        // the owner decision is read without the hub's lock (an `Owners` may consult the hub); whether anybody
        // watches is read under the same lock as the removal, so a subscription that lands meanwhile either keeps
        // this poller or finds it gone and starts a new one
        let delegated = self.0.owners.delegate(person, &conversation).is_some();
        let mut st = self.st();
        if st.drained {
            return true;
        }
        if st.timeline.contains_key(&key) && !delegated {
            *idle_since = None;
            return false;
        }
        let since = *idle_since.get_or_insert_with(Instant::now);
        if since.elapsed() >= linger {
            st.timeline_pollers.remove(&key);
            return true;
        }
        false
    }

    /// A timeline poller stopped on its own (the conversation is gone): forget it.
    pub(crate) fn timeline_poller_gone(&self, person: &str, conversation: Uuid) {
        self.st().timeline_pollers.remove(&(person.to_owned(), conversation));
    }

    /// M1 for a person, with one magpie call per (person, cursor, limit) in [`LIST_CACHE`] (7.5.2: the two list
    /// requests of a connect share it).
    /// The call carries the socket's identity (the viewer headers of a view-as socket), and a view-as socket shares
    /// answers only with sockets of the same view session. A failed answer is not kept: the next request asks again.
    pub(crate) async fn list(
        &self,
        app: &App,
        conn: &Conn,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<ListResponse, Fail> {
        let viewer = conn.viewer.as_ref().map(|v| (v.admin.clone(), v.session.clone()));
        let key = (conn.person.clone(), viewer, cursor.clone(), limit);
        let (asked, cell) = {
            let mut st = self.st();
            st.lists.retain(|_, (at, _)| at.elapsed() < LIST_CACHE);
            let (at, cell) = st
                .lists
                .entry(key.clone())
                .or_insert_with(|| (Instant::now(), Arc::new(OnceCell::new())));
            (*at, cell.clone())
        };
        let r = cell
            .get_or_init(|| async {
                let q = ListQuery {
                    state: None,
                    limit: Some(limit),
                    cursor,
                    search: None,
                };
                conn.calls(&app.0.magpie).list_conversations(&q).await.map_err(fail)
            })
            .await
            .clone();
        if let Ok(list) = &r {
            // as of the shared call's start (an answer that started before a release says what is no longer so)
            app.legacy()
                .observe_all(&conn.person, &list.conversations, crate::legacy::Seen::Read(asked));
        }
        if r.is_err() {
            // callers already waiting on this cell share the failure; later ones call again
            let mut st = self.st();
            if st.lists.get(&key).is_some_and(|(_, c)| Arc::ptr_eq(c, &cell)) {
                st.lists.remove(&key);
            }
        }
        r
    }

    /// Spec 11.2 step 3 (and its request-free-first rule, DESIGN 9.3): every socket is closed 1012 at a moment
    /// within `jitter`, request-free sockets first (no request in flight, no message arriving, nothing but
    /// heartbeats for 5 s) in the first half, the rest in the second half once their in-flight requests have
    /// answered and their arriving messages have been handled (or `rpc_wait` passed). The 1012 follows every frame
    /// already queued for the socket (the reply it waited for, rows), and waits again for a request or a send that
    /// began while those were written ([`Conn::close_for_drain`]). A socket with a transfer (a large send arriving,
    /// or read and not yet handled) is waited for up to `transfer_bound` from the start of the drain instead (spec
    /// 11.2 step 3, 13). Until the sockets are gone the pollers keep feeding them; then the pollers stop (step 5).
    /// Returns when every socket is gone, or at `transfer_bound`; a socket that registers after the drain began is
    /// closed 1012 at once ([`Self::register`]).
    pub async fn drain(&self, jitter: Duration, rpc_wait: Duration, transfer_bound: Duration) {
        let conns: Vec<Arc<Conn>> = {
            let mut st = self.st();
            st.draining = true;
            st.conns.values().cloned().collect()
        };
        let started = Instant::now();
        let transfer_deadline = tokio::time::Instant::now() + transfer_bound;
        let half = jitter / 2;
        let mut first: Vec<Arc<Conn>> = Vec::new();
        let mut later: Vec<Arc<Conn>> = Vec::new();
        for c in conns {
            if c.request_free() {
                first.push(c);
            } else {
                later.push(c);
            }
        }
        let mut tasks = tokio::task::JoinSet::new();
        for c in first {
            let at = random_within(half);
            tasks.spawn(async move {
                tokio::time::sleep(at).await;
                if !c.request_free() {
                    c.wait_requests(rpc_wait, transfer_deadline).await;
                }
                c.close_for_drain(rpc_wait, transfer_deadline);
            });
        }
        for c in later {
            let at = half + random_within(jitter.saturating_sub(half));
            tasks.spawn(async move {
                tokio::time::sleep(at).await;
                c.wait_requests(rpc_wait, transfer_deadline).await;
                c.close_for_drain(rpc_wait, transfer_deadline);
            });
        }
        while tasks.join_next().await.is_some() {}
        // every socket has been asked to close: give the last ones a moment to answer, longer while one is still busy
        // (its 1012 waits for a send that began while its queue was being written), never past the bound
        let deadline = (started + jitter + rpc_wait).max(Instant::now() + Duration::from_secs(3));
        let bound = started + transfer_bound;
        while self.sockets() > 0
            && Instant::now() < bound
            && (Instant::now() < deadline || self.st().conns.values().any(|c| c.busy()))
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // step 5: the pollers stop (their control channels close)
        let mut st = self.st();
        st.drained = true;
        st.change_pollers.clear();
        st.timeline_pollers.clear();
    }
}

fn random_within(d: Duration) -> Duration {
    if d.is_zero() {
        return d;
    }
    let r = Uuid::new_v4().as_u128();
    #[allow(clippy::cast_possible_truncation)] // a fraction of a short duration in nanoseconds
    let nanos = (r % d.as_nanos().max(1)) as u64;
    Duration::from_nanos(nanos)
}

/// The exponential backoff of the pollers (8.2): 0.5 s doubling to 5 s, for ever.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    next: Duration,
    min: Duration,
    max: Duration,
}

impl Backoff {
    /// From `min` doubling to `max`.
    #[must_use]
    pub fn new(min: Duration, max: Duration) -> Self {
        Self { next: min, min, max }
    }

    /// The wait before the next attempt.
    pub fn wait(&mut self) -> Duration {
        let d = self.next;
        self.next = (self.next * 2).min(self.max);
        d
    }

    /// A success: start from `min` again.
    pub fn reset(&mut self) {
        self.next = self.min;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests

    use super::*;

    /// A socket whose upgrade was in flight when the drain took its list registers after it: it is closed 1012 at
    /// once (after anything queued), so it neither holds the drain to its deadline nor is cut by the exit.
    #[tokio::test]
    async fn a_socket_registering_after_the_drain_began_is_closed_1012() {
        let hub = Hub::default();
        hub.drain(Duration::ZERO, Duration::from_millis(10), Duration::from_secs(1))
            .await;
        let (conn, (mut out, _close)) = Conn::detached("staff@example.com");
        hub.register(&conn);
        assert!(conn.is_closing());
        let queued = out.try_recv().unwrap();
        assert!(queued.ends_with("close:1012:service restart"), "{queued:?}");
        // and before the drain, a socket registers open
        let hub = Hub::default();
        let (conn, (mut out, _close)) = Conn::detached("staff@example.com");
        hub.register(&conn);
        assert!(!conn.is_closing());
        assert!(out.try_recv().is_err());
    }

    /// The linger check and the removal read the watchers under one lock: a poller whose linger has run out keeps
    /// running when a socket watches the conversation by then, and goes only when nobody does.
    #[test]
    fn a_timeline_poller_past_its_linger_stays_while_watched() {
        let hub = Hub::default();
        let person = "staff@example.com";
        let conversation = Uuid::new_v4();
        let key = (person.to_owned(), conversation);
        let (tx, _rx) = mpsc::unbounded_channel();
        let linger = Duration::from_millis(100);
        let past = Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
        {
            let mut st = hub.st();
            st.timeline_pollers.insert(key.clone(), tx);
            st.timeline.entry(key.clone()).or_default().insert(Uuid::new_v4());
        }
        let mut idle = Some(past);
        assert!(!hub.timeline_poller_done(person, conversation, &mut idle, linger));
        assert_eq!(idle, None, "a watched poller is not idle");
        assert!(hub.st().timeline_pollers.contains_key(&key));
        // nobody watches: idle from now, done once the linger has passed
        hub.st().timeline.remove(&key);
        assert!(!hub.timeline_poller_done(person, conversation, &mut idle, linger));
        assert!(idle.is_some());
        assert!(hub.st().timeline_pollers.contains_key(&key));
        let mut idle = Some(past);
        assert!(hub.timeline_poller_done(person, conversation, &mut idle, linger));
        assert!(!hub.st().timeline_pollers.contains_key(&key));
    }

    /// The race itself: a poller past its linger checks whether it is done while a socket subscribes on another
    /// thread, the subscription doing what [`Hub::set_timeline`] and [`Hub::ensure_timeline_poller`] do (the watcher
    /// added under one lock, then a poller started under another if none runs). Whatever the interleaving, a watched
    /// conversation is left with a poller: either the check saw the watcher and kept its poller, or it removed the
    /// poller first and the subscription started a new one. A check that reads the watchers under one lock and
    /// removes under another loses the poller when the subscription lands in between.
    #[test]
    fn a_subscription_racing_the_linger_check_always_ends_with_a_poller() {
        let hub = Hub::default();
        let person = "staff@example.com";
        let linger = Duration::from_millis(1);
        let past = Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
        let rounds = 60_000;
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let mut lost = 0;
        for _ in 0..rounds {
            let conversation = Uuid::new_v4();
            let key = (person.to_owned(), conversation);
            let (tx, _rx) = mpsc::unbounded_channel();
            hub.st().timeline_pollers.insert(key.clone(), tx);
            let subscriber = {
                let (hub, barrier, key) = (hub.clone(), barrier.clone(), key.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    hub.st().timeline.entry(key.clone()).or_default().insert(Uuid::new_v4());
                    let mut st = hub.st();
                    st.timeline_pollers
                        .entry(key)
                        .or_insert_with(|| mpsc::unbounded_channel().0);
                })
            };
            barrier.wait();
            let mut idle = Some(past);
            let _ = hub.timeline_poller_done(person, conversation, &mut idle, linger);
            subscriber.join().unwrap();
            let st = hub.st();
            if st.timeline.contains_key(&key) && !st.timeline_pollers.contains_key(&key) {
                lost += 1;
            }
        }
        assert_eq!(
            lost, 0,
            "{lost} of {rounds} watched conversations were left without a poller"
        );
    }
}
