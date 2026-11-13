//! The FUSE server (supervisor 9.3-9.10): one worker per journal slot reading requests straight into its slot,
//! each request answered from the tree, the content cache, the backing directory or magpie; the resend rule of 9.3
//! (a checkpoint's resend applies once); the journal replay after a restart (9.9); the lease state and the repair.

#![allow(
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::match_same_arms
)] // FUSE argument names: h(eader), b(ody), p(ath)

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, RwLock};
use std::time::Duration;

use axum::body::Bytes;
use magpie_api::sandbox::{CcFileQuery, CcKind, CcMkdir, CcPathOp, CcRename, CcSetattr, CcTruncate, CcWriteQuery};
use sha2::Digest;
use tokio::runtime::Handle;
use uuid::Uuid;

use super::cache::{self, Candidate, Content, Entry, Tree};
use super::errno::{
    EACCES, EEXIST, EFBIG, EINTR, EINVAL, EIO, ENODATA, ENOENT, ENOSYS, ENOTDIR, ENOTEMPTY, ENOTSUP, EOPNOTSUPP, EPERM,
    EXDEV,
};
use super::journal::{Journal, KEPT_SLOTS, KeptRecord};
use super::local::{Local, SetAttr, fh};
use super::nodes::{self, Nodes, ROOT};
use super::proto::{self, Attr, FileType, IN_HEADER, InHeader, cstr, fattr, op, u32_at, u64_at};
use super::remote::{CallError, Lease, OpCtl, Remote};
use super::scope::{self, Scope, Unanswered, Unclassified};
use super::seen::{self, Seen};
use super::{Config, Device, Hooks, Point, StuckOp, now_ns};

/// A replayed local-scope mutation whose effect is already there (9.9): the killed instance made it and marked its
/// slot done ([`Journal::mark_done`], right after the change, before the reply), so the replay answers success. A
/// replay that finds the state but no mark answers the error as any request would (the change was never made by this
/// request: an unlink of a missing file stays ENOENT, a `mkdir` lock another process took meanwhile stays EEXIST).
/// The limit: a kill between the change and the mark (two adjacent instructions) answers the error for a change
/// that was this request's own.
fn replayed_as_done(h: &InHeader, path: &str) {
    tracing::info!(
        unique = h.unique,
        op = proto::opname(h.opcode),
        path,
        "journal replay: the local change was already made"
    );
}

thread_local! {
    /// The journal slot of the request this thread serves (a worker's, or the replay's), for the done marks.
    static SLOT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// A worker that "died" at a fault point (tests without the `fault-injection` feature): unwinds the worker.
struct Died;

/// The outcome of one request.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Outcome {
    errno: i32,
    payload: Vec<u8>,
    /// The request made (or may have made) a change: a magpie call, a local mutation. An EINTR before any magpie
    /// attempt has none.
    effect: bool,
}

impl Outcome {
    fn of(r: Result<Vec<u8>, i32>, effect: bool) -> Self {
        match r {
            Ok(p) => Self {
                errno: 0,
                payload: p,
                effect,
            },
            Err(e) => Self {
                errno: e,
                payload: Vec::new(),
                effect: effect && e != EINTR,
            },
        }
    }
}

type Key = (u32, u64, [u8; 32]);

struct Inflight {
    key: Key,
    /// A resend took this request's result (set under the resend lock).
    matched: AtomicBool,
    done: Mutex<Option<Outcome>>,
    cv: Condvar,
}

#[derive(Default)]
struct Resend {
    inflight: HashMap<u32, Vec<Arc<Inflight>>>,
    /// pid → (kept slot, record, order, since: when it was kept, restored or the server resumed).
    kept: HashMap<u32, (usize, KeptRecord, u64, std::time::Instant)>,
    order: u64,
}

struct State {
    nodes: Nodes,
    tree: Tree,
    /// Entries of magpie nodes unlinked while open (kept until the next start, 9.8).
    gone: HashMap<u64, Entry>,
}

/// The server.
pub struct Core {
    cfg: Config,
    rt: Handle,
    dev: Arc<dyn Device>,
    journal: Journal,
    boot: Uuid,
    remote: Arc<Remote>,
    conversation: Uuid,
    st: Mutex<State>,
    content: Mutex<Content>,
    local: Local,
    main: Mutex<Option<String>>,
    turn: Mutex<Option<Uuid>>,
    unclassified: Unclassified,
    unanswered: Unanswered,
    resend: Mutex<Resend>,
    ctls: Mutex<HashMap<u64, Arc<OpCtl>>>,
    inflight: AtomicU32,
    idle: (Mutex<()>, Condvar),
    hooks: RwLock<Hooks>,
    relocking: AtomicBool,
    /// A write answered EDQUOT (the heartbeat's `cc.quotaExceeded`, 9.10).
    quota_exceeded: AtomicBool,
    /// The uuids of transcript lines magpie acknowledged (8.2), and its condition variable.
    seen: (Mutex<Seen>, Condvar),
    /// How often the one-stuck-operation rule fired (9.5 item 4).
    stuck: AtomicU64,
    dead: AtomicBool,
    stopped: AtomicBool,
    started_ns: i64,
    /// Whole-file fetches in progress, one per node (9.7).
    fetches: Mutex<HashMap<u64, Arc<Fetch>>>,
    /// Reads that left no whole request in the slot (`fuse.short_read`, 9.9).
    short_reads: AtomicU64,
    /// Short reads whose request no copy of the header names (`fuse.request_lost`): no reply was possible.
    lost_requests: AtomicU64,
    /// The first request of this instance was logged (its `unique`: the rig's monotonicity check across restarts).
    first_logged: AtomicBool,
}

/// A whole-file fetch into the content cache (9.7): streamed into its staging file in the background; READs of
/// ranges that already arrived are answered from that file while the rest streams, and every READ of the node waits
/// on this one fetch instead of starting its own.
struct Fetch {
    version: u64,
    size: u64,
    staging: std::path::PathBuf,
    arrived: AtomicU64,
    done: Mutex<Option<Result<std::path::PathBuf, i32>>>,
    cv: Condvar,
}

/// What a READ waiting on a fetch gets.
enum Got {
    /// The asked range arrived: read it from the staging file.
    Range(std::path::PathBuf),
    /// The fetch ended: the cache file (at the version the fetch ended at), or why not.
    Done(Result<std::path::PathBuf, i32>),
}

impl Fetch {
    /// Waits until `upto` bytes arrived or the fetch ended.
    fn wait(&self, upto: u64) -> Got {
        let mut g = lock(&self.done);
        loop {
            if let Some(r) = g.clone() {
                return Got::Done(r);
            }
            if self.arrived.load(Ordering::SeqCst) >= upto {
                return Got::Range(self.staging.clone());
            }
            g = self.cv.wait(g).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn progress(&self, n: u64) {
        self.arrived.fetch_add(n, Ordering::SeqCst);
        let _g = lock(&self.done);
        self.cv.notify_all();
    }
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Core").field("boot", &self.boot).finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How a re-send of the suffix magpie lost ended (9.5 item 6).
enum Resent {
    /// magpie holds the whole cached copy now, at this version.
    Done(u64),
    /// magpie holds at least as many bytes as the cache: nothing to re-send.
    NotAhead,
    /// magpie's bytes are not a prefix of the cache's (the `forked` hook ran).
    Forked,
    /// Not finished for a reason that proves nothing about the bytes (a read or a write that failed: unavailable,
    /// stuck, quota, lease); `advanced` is magpie's version when some writes were stored.
    Failed { advanced: Option<u64> },
}

/// What [`Core::start`] needs besides the configuration.
#[allow(missing_debug_implementations)] // a device and hooks
pub struct StartArgs {
    pub dev: Arc<dyn Device>,
    pub boot: Uuid,
    /// The connection is the keeper's (a restart): replay the journal, keep the node ids and kept results.
    pub restart: bool,
    pub remote: Arc<Remote>,
    pub lease: magpie_api::sandbox::LeaseResponse,
    pub hooks: Hooks,
}

impl Core {
    /// Loads the node ids, the tree and the cache, repairs what magpie lost, replays the journal (a restart), then
    /// starts the workers. Blocking (call it off the runtime); `rt` runs the magpie calls.
    ///
    /// # Errors
    /// A local file (journal, node log, cache, backing directory) cannot be opened.
    pub fn start(cfg: Config, rt: Handle, a: StartArgs) -> std::io::Result<Arc<Self>> {
        std::fs::create_dir_all(&cfg.state)?;
        let journal = Journal::open(&cfg.journal, cfg.slots, cfg.slot_bytes)?;
        let nl = nodes::log_path(&cfg.state);
        if !a.restart {
            // a new connection: the sentry holds none of the old ids, uniques or results
            for i in 0..journal.slots() {
                journal.free(i);
            }
            journal.clear_kept();
            let _ = std::fs::remove_file(&nl);
        }
        let nodes = Nodes::open(&nl)?;
        let content = Content::open(&cfg.cache, cfg.cache_budget)?;
        let local = Local::open(&cfg.local)?;
        let core = Arc::new(Self {
            rt,
            dev: a.dev,
            journal,
            boot: a.boot,
            remote: a.remote,
            conversation: a.lease.conversation_id,
            st: Mutex::new(State {
                nodes,
                tree: Tree::default(),
                gone: HashMap::new(),
            }),
            content: Mutex::new(content),
            local,
            main: Mutex::new(a.lease.main_transcript.clone()),
            turn: Mutex::new(None),
            unclassified: Unclassified::default(),
            unanswered: Unanswered::default(),
            resend: Mutex::new(Resend::default()),
            ctls: Mutex::new(HashMap::new()),
            inflight: AtomicU32::new(0),
            idle: (Mutex::new(()), Condvar::new()),
            hooks: RwLock::new(a.hooks),
            relocking: AtomicBool::new(false),
            quota_exceeded: AtomicBool::new(false),
            seen: (Mutex::new(Seen::default()), Condvar::new()),
            stuck: AtomicU64::new(0),
            dead: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            started_ns: now_ns(),
            fetches: Mutex::new(HashMap::new()),
            short_reads: AtomicU64::new(0),
            lost_requests: AtomicU64::new(0),
            first_logged: AtomicBool::new(false),
            cfg,
        });
        core.remote.set_lease(Some(Lease {
            id: a.lease.lease_id,
            epoch: a.lease.epoch,
        }));
        if a.lease.resync {
            // magpie 6.12: the reconcile of this tree load re-sends every suffix magpie lacks (it runs at every tree
            // load, so `resync: true` needs nothing more than this load)
            tracing::warn!("the lease asks for a resync: the cache's suffixes magpie lacks are re-sent now");
        }
        // Never replay or drop node ids against a tree that was not loaded: an empty tree would answer ENOENT to
        // replays magpie already applied and drop every magpie-scope node the sentry holds. The fetch waits through
        // magpie's unavailability (the retry rule); a definitive failure fails the start, and the mount is retried
        // on the keeper's connection.
        // The tree load's repairs and the replay run before the heartbeat starts (4.3 step 7): while they run, the
        // probe gives the stuck rule its evidence that magpie answers others (9.5 item 4).
        let _probe = core.remote.health_probe(&core.rt);
        if let Err(e) = core.fetch_tree() {
            tracing::error!(error = ?e, "the tree could not be loaded: the start fails");
            return Err(std::io::Error::other(format!("cc/tree: {e:?}")));
        }
        if a.restart {
            core.restore_kept();
            core.replay();
        }
        core.retain_nodes();
        core.spawn_workers();
        // 9.7's prefetch after every start, not only a fresh connection's: a first start that failed is retried on
        // the keeper's connection, and a restart's reconcile may have dropped the main transcript's entry (a crash
        // between magpie's 200 and the cache update); `prefetch_main` does nothing while the entry is current
        let c = core.clone();
        std::thread::spawn(move || c.prefetch_main());
        Ok(core)
    }

    // ---- hooks, lease, settle ----

    /// Replaces the hooks (the binary wires the agent's after both exist).
    pub fn set_hooks(&self, h: Hooks) {
        *self.hooks.write().unwrap_or_else(PoisonError::into_inner) = h;
    }

    fn hooks(&self) -> Hooks {
        self.hooks.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The turn the agent is running (each write carries it as `turnId`).
    pub fn set_turn(&self, t: Option<Uuid>) {
        *lock(&self.turn) = t;
    }

    /// The main transcript's path (from the `running` report): pinned in the content cache.
    pub fn set_main_transcript(&self, p: Option<String>) {
        let node = p.as_deref().and_then(|p| lock(&self.st).nodes.id_of(p));
        lock(&self.content).pin(node);
        *lock(&self.main) = p;
    }

    #[must_use]
    pub fn remote(&self) -> &Arc<Remote> {
        &self.remote
    }

    #[must_use]
    pub fn boot(&self) -> Uuid {
        self.boot
    }

    /// Whether the last state change magpie answered was refused for the quota (the heartbeat reports it).
    #[must_use]
    pub fn quota_exceeded(&self) -> bool {
        self.quota_exceeded.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn inflight(&self) -> u32 {
        self.inflight.load(Ordering::SeqCst)
    }

    /// Waits until no request is being handled, at most `d`; whether none is.
    #[must_use]
    pub fn settle(&self, d: Duration) -> bool {
        let deadline = std::time::Instant::now() + d;
        let mut g = lock(&self.idle.0);
        while self.inflight() > 0 {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            g = self
                .idle
                .1
                .wait_timeout(g, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    /// How often the one-stuck-operation rule fired.
    #[must_use]
    pub fn stuck_count(&self) -> u64 {
        self.stuck.load(Ordering::SeqCst)
    }

    /// Waits at most `max` until magpie holds the transcript lines of `uuids` (8.2): `Some(true)` once it does (or
    /// the server stopped: nothing more will be stored), `Some(false)` once the stuck rule fires after
    /// `stuck_before`, `None` when `max` passed first (the caller waits again, or gives up: the wait has no cap of
    /// its own). With `scan`, uuids not seen in this server's writes are first looked for in the last 1 MiB of the
    /// main transcript as magpie holds it (writes answered before a restart). Blocking.
    #[must_use]
    pub fn wait_uuids(&self, uuids: &[String], stuck_before: u64, scan: bool, max: Duration) -> Option<bool> {
        if scan && !lock(&self.seen.0).holds(uuids) {
            self.scan_main_tail();
        }
        let until = std::time::Instant::now() + max;
        let mut g = lock(&self.seen.0);
        loop {
            if g.holds(uuids) {
                return Some(true);
            }
            if self.stopped.load(Ordering::SeqCst) {
                return Some(true);
            }
            if self.stuck_count() > stuck_before {
                return Some(false);
            }
            let left = until.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            g = self
                .seen
                .1
                .wait_timeout(g, left.min(Duration::from_secs(1)))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The stuck rule fired (tests).
    #[cfg(test)]
    pub fn stuck_for_test(&self) {
        self.stuck.fetch_add(1, Ordering::SeqCst);
        self.seen.1.notify_all();
    }

    fn scan_main_tail(&self) {
        let Some(m) = lock(&self.main).clone() else { return };
        let Ok(e) = self.magpie_entry(&m) else { return };
        if e.size == 0 {
            return;
        }
        let from = e.size.saturating_sub(seen::TAIL_SCAN);
        match self.fetch_range(&OpCtl::default(), &m, from, e.size - from) {
            Ok(b) => {
                let mut s = lock(&self.seen.0);
                for u in seen::uuids_in(&b) {
                    s.add(u);
                }
            }
            Err(errno) => tracing::info!(errno, "the main transcript's tail could not be scanned"),
        }
    }

    /// Stops the workers (after an unmount; tests: the device's close).
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.dev.close();
    }

    /// Whether a fault point "killed" this instance (tests).
    #[must_use]
    pub fn died(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    fn on_lease_lost(self: &Arc<Self>) {
        if self.relocking.swap(true, Ordering::SeqCst) {
            return;
        }
        tracing::warn!("lease lost: every magpie-scope request answers EIO until a new lease");
        (self.hooks().lease_lost)();
        let c = self.clone();
        self.rt.spawn(async move {
            let lease = crate::magpie::take_lease(c.remote.client(), c.conversation, |w| {
                tracing::info!(wait = ?w, "lease not granted yet");
            })
            .await;
            let c2 = c.clone();
            let l2 = lease.clone();
            let _ = tokio::task::spawn_blocking(move || {
                // the new lease first, so the tree load's repair writes carry it; requests keep answering EIO
                // until `relocking` is cleared (`leased`)
                c2.remote.set_lease(Some(Lease {
                    id: l2.lease_id,
                    epoch: l2.epoch,
                }));
                lock(&c2.main).clone_from(&l2.main_transcript);
                c2.load_tree();
            })
            .await;
            c.relocking.store(false, Ordering::SeqCst);
            tracing::info!(epoch = lease.epoch, "lease taken again");
            (c.hooks().lease_regained)(lease);
        });
    }

    /// After a restore (or a clock jump): re-load the tree and revalidate the cache. The kept interrupted results
    /// (9.3) count their time from now: the thread's resend follows the restore, however long the pause was.
    pub fn resumed(self: &Arc<Self>) {
        {
            let mut rs = lock(&self.resend);
            for v in rs.kept.values_mut() {
                v.3 = std::time::Instant::now();
            }
        }
        self.load_tree();
    }

    /// Reads that left no whole request in their slot since start (`fuse.short_read`).
    #[must_use]
    pub fn short_reads(&self) -> u64 {
        self.short_reads.load(Ordering::SeqCst)
    }

    /// Short reads whose request could not be answered at all (no copy of its header, `fuse.request_lost`).
    #[must_use]
    pub fn lost_requests(&self) -> u64 {
        self.lost_requests.load(Ordering::SeqCst)
    }

    // ---- the tree, the cache, the repair ----

    /// A new tree from magpie; node ids are dropped only against a tree that loaded (else the old tree stays).
    fn load_tree(self: &Arc<Self>) {
        match self.fetch_tree() {
            Ok(()) => self.retain_nodes(),
            Err(e) => tracing::error!(error = ?e, "the tree could not be re-loaded: the previous one is kept"),
        }
    }

    /// Drops the node ids of paths that no longer exist (after the tree load and, on a restart, the replay: a
    /// replayed rename moves its node first).
    fn retain_nodes(&self) {
        let mut st = lock(&self.st);
        let State { nodes, tree, .. } = &mut *st;
        let local = &self.local;
        nodes.retain(|p| match scope::scope_of(p) {
            Scope::Magpie => tree.get(p).is_some(),
            Scope::Local => local.exists(p),
            Scope::Top | Scope::Root => true,
            Scope::Refused => false,
        });
    }

    fn fetch_tree(self: &Arc<Self>) -> Result<(), CallError> {
        let ctl = OpCtl::default();
        let t = self.rt.block_on(self.remote.read(
            &ctl,
            "cc/tree",
            |c| async move { c.client.sandbox(None).cc_tree().await },
            &|| {},
        ))?;
        let tree = Tree::from_magpie(&t);
        let main = lock(&self.main).clone();
        let candidates = {
            let mut st = lock(&self.st);
            st.tree = tree;
            let mut content = lock(&self.content);
            content.pin(main.as_deref().and_then(|m| st.nodes.id_of(m)));
            let State { nodes, tree, .. } = &*st;
            content.reconcile(|node| {
                let (p, gone) = nodes.get(node)?;
                if gone {
                    return None;
                }
                tree.get(p).filter(|e| !e.is_dir()).map(|e| (e.version, e.size))
            })
        };
        for c in candidates {
            self.repair_candidate(&c);
        }
        Ok(())
    }

    /// 9.5 item 6 / magpie 6.12: a cache file newer and longer than magpie's copy whose first bytes equal magpie's
    /// is re-sent from magpie's end with fresh `opId`s. (A repeated repair after a crash is harmless: magpie answers
    /// a write whose range already holds those bytes with success, rule 0.)
    fn repair_candidate(self: &Arc<Self>, c: &Candidate) -> bool {
        let (path, entry) = {
            let st = lock(&self.st);
            let Some((p, false)) = st.nodes.get(c.node) else {
                drop(st);
                lock(&self.content).discard(c);
                return false;
            };
            let Some(e) = st.tree.get(p).copied() else {
                let p = p.to_owned();
                drop(st);
                tracing::error!(path = %p, "repair: magpie has no such file");
                lock(&self.content).discard(c);
                return false;
            };
            (p.to_owned(), e)
        };
        let file = lock(&self.content).candidate_file(c);
        match self.resend_suffix(&path, &file, entry, c.size) {
            Resent::Done(v) => {
                lock(&self.content).adopt(c, v);
                true
            }
            Resent::NotAhead | Resent::Forked => {
                lock(&self.content).discard(c);
                false
            }
            Resent::Failed { advanced } => {
                // not a proven mismatch: the only copy of what magpie lost stays for the next tree load or lease
                tracing::warn!(path = %path, "repair not finished: the cached copy is kept for the next tree load");
                if let Some(v) = advanced {
                    lock(&self.content).rekey_candidate(c, v);
                }
                false
            }
        }
    }

    /// Compares magpie's last 64 KiB with the cached copy and re-sends `[magpie.size, local)`; magpie's new version.
    /// Each write is an append at magpie's size (`append`: a size magpie does not have answers `offset_mismatch`,
    /// never an overwrite). Its `opId` is a fresh v4 UUID not recorded first (a deviation from 9.5 item 6's "recorded
    /// in `run.json` before sending"): a crash during a repair repeats it after the next tree load, and magpie
    /// answers a repeated write whose range already holds those bytes with success (rule 0), or a write at the size
    /// it now has as an ordinary append. A `lease_lost` here runs the lease loop as any request's does.
    fn resend_suffix(self: &Arc<Self>, path: &str, file: &std::path::Path, e: Entry, local: u64) -> Resent {
        if e.size >= local {
            return Resent::NotAhead;
        }
        let from = e.size.saturating_sub(64 * 1024);
        let ctl = OpCtl::default();
        let theirs = match self.fetch_range(&ctl, path, from, e.size - from) {
            Ok(b) => b,
            Err(errno) => {
                tracing::warn!(path, errno, "repair: magpie's bytes could not be read");
                return Resent::Failed { advanced: None };
            }
        };
        #[allow(clippy::cast_possible_truncation)] // 64 KiB
        let ours = match cache::read_at(file, from, (e.size - from) as usize) {
            Ok(b) => b,
            Err(err) => {
                tracing::warn!(path, error = %err, "repair: the cached copy could not be read");
                return Resent::Failed { advanced: None };
            }
        };
        if theirs != ours {
            tracing::error!(
                path,
                magpie_size = e.size,
                local_size = local,
                version = e.version,
                "the cached copy disagrees with magpie: the history forked"
            );
            (self.hooks().forked)(format!(
                "{path}: cached copy disagrees with magpie at {from}..{}",
                e.size
            ));
            return Resent::Forked;
        }
        let mut off = e.size;
        let mut version = e.version;
        let advanced = |v: u64| Resent::Failed {
            advanced: (v != e.version).then_some(v),
        };
        while off < local {
            #[allow(clippy::cast_possible_truncation)] // a chunk is at most 1 MiB
            let n = ((local - off) as usize).min(self.cfg.max_write as usize);
            let Ok(data) = cache::read_at(file, off, n) else {
                tracing::warn!(path, offset = off, "repair: the cached copy could not be read");
                return advanced(version);
            };
            let data = Bytes::from(data);
            let q = CcWriteQuery {
                path: path.to_owned(),
                offset: off,
                mtime_ns: now_ns(),
                op_id: Uuid::new_v4(),
                create: false,
                mode: e.mode,
                turn_id: None,
                append: true,
            };
            let r = self.rt.block_on(self.remote.call(
                &ctl,
                "cc/write (repair)",
                |c| {
                    let q = q.clone();
                    let d = data.clone();
                    async move { c.sb().cc_write(&q, d).await }
                },
                &|| {},
            ));
            self.after_call("cc/write (repair)", &r);
            match r {
                Ok(sv) => {
                    version = sv.version;
                    off += n as u64;
                }
                Err(err) => {
                    tracing::error!(path, offset = off, error = ?err, "repair write failed");
                    return advanced(version);
                }
            }
        }
        tracing::warn!(path, from = e.size, to = local, "repaired a suffix magpie had lost");
        let mut st = lock(&self.st);
        if let Some(mut ent) = st.tree.get(path).copied() {
            ent.size = local;
            ent.version = version;
            st.tree.insert(path, ent);
        }
        Resent::Done(version)
    }

    fn fetch_range(&self, ctl: &OpCtl, path: &str, offset: u64, len: u64) -> Result<Vec<u8>, i32> {
        let q = CcFileQuery {
            path: path.to_owned(),
            offset: Some(offset),
            length: Some(len),
        };
        self.rt
            .block_on(self.remote.read(
                ctl,
                "cc/file",
                |c| {
                    let q = q.clone();
                    async move {
                        let r = c.client.sandbox(None).cc_file(&q).await?;
                        r.bytes().await.map_err(magpie_api::client::ClientError::Http)
                    }
                },
                &|| {},
            ))
            .map(|b| b.to_vec())
            .map_err(|e| e.errno())
    }

    /// The node's whole-file fetch: the one in progress, or a new one started in the background (9.7).
    fn fetch(self: &Arc<Self>, node: u64, path: &str, e: Entry) -> Arc<Fetch> {
        let mut fs = lock(&self.fetches);
        if let Some(f) = fs.get(&node) {
            return f.clone();
        }
        let staging = {
            let mut c = lock(&self.content);
            c.begin_fetch(node);
            c.staging(node)
        };
        let f = Arc::new(Fetch {
            version: e.version,
            size: e.size,
            staging,
            arrived: AtomicU64::new(0),
            done: Mutex::new(None),
            cv: Condvar::new(),
        });
        fs.insert(node, f.clone());
        drop(fs);
        let (c, f2, path) = (self.clone(), f.clone(), path.to_owned());
        self.rt.spawn(async move {
            let r = c.run_fetch(&f2, &path).await;
            let c2 = c.clone();
            let _ = tokio::task::spawn_blocking(move || c2.finish_fetch(node, &f2, r)).await;
        });
        f
    }

    /// Streams the file into the staging file. Each wait (the answer, every next chunk) is bounded by the attempt
    /// timeout, not the whole body, and a retry resumes from the bytes that arrived (a range from there) while the
    /// file is still at the fetched version.
    async fn run_fetch(self: &Arc<Self>, f: &Arc<Fetch>, path: &str) -> Result<u64, CallError> {
        use super::remote::AttemptError;
        let t = self.remote.attempt_timeout();
        let me = self.clone();
        self.remote
            .read_streamed(
                &OpCtl::default(),
                "cc/file",
                |c| {
                    let (f, path, me) = (f.clone(), path.to_owned(), me.clone());
                    async move {
                        let have = f.arrived.load(Ordering::SeqCst);
                        if have >= f.size {
                            return Ok(have);
                        }
                        if have > 0 && me.magpie_entry(&path).map(|e| e.version) != Ok(f.version) {
                            // the file changed since the fetch began: a range would mix two versions
                            return Err(AttemptError::Local(EIO));
                        }
                        let q = CcFileQuery {
                            path,
                            offset: (have > 0).then_some(have),
                            length: (have > 0).then(|| f.size - have),
                        };
                        let mut r = tokio::time::timeout(t, c.client.sandbox(None).cc_file(&q))
                            .await
                            .map_err(|_| AttemptError::TimedOut)??;
                        let mut file = std::fs::OpenOptions::new()
                            .create(true)
                            .write(true)
                            .truncate(false)
                            .open(&f.staging)
                            .and_then(|file| file.set_len(have).map(|()| file))
                            .map_err(|e| {
                                tracing::warn!(error = %e, "the cache file cannot be written");
                                AttemptError::Local(EIO)
                            })?;
                        use std::io::{Seek, Write};
                        file.seek(std::io::SeekFrom::Start(have))
                            .map_err(|_| AttemptError::Local(EIO))?;
                        loop {
                            let chunk = tokio::time::timeout(t, r.chunk())
                                .await
                                .map_err(|_| AttemptError::TimedOut)?
                                .map_err(|e| AttemptError::Client(magpie_api::client::ClientError::Http(e)))?;
                            let Some(chunk) = chunk else { break };
                            file.write_all(&chunk).map_err(|_| AttemptError::Local(EIO))?;
                            f.progress(chunk.len() as u64);
                        }
                        Ok(f.arrived.load(Ordering::SeqCst))
                    }
                },
                &|| f.arrived.load(Ordering::SeqCst),
            )
            .await
    }

    /// The fetch ended: the staging file becomes the cache entry, with the writes, truncates and renames magpie
    /// acknowledged while it streamed applied to it, so the entry is current when the fetch ends (the pinned main
    /// transcript is never left uncached by a write during its prefetch).
    fn finish_fetch(&self, node: u64, f: &Fetch, r: Result<u64, CallError>) {
        let res = match r {
            Ok(n) if n == f.size => {
                let st = lock(&self.st);
                let cur = st
                    .nodes
                    .get(node)
                    .filter(|(_, gone)| !gone)
                    .and_then(|(p, _)| st.tree.get(p))
                    .map(|e| (e.version, e.size));
                let mut c = lock(&self.content);
                match c.fill_fetched(node, f.version, f.size, &f.staging) {
                    Some(got) if Some(got) == cur => c.valid(node, got.0, got.1).ok_or(EIO),
                    Some(got) => {
                        tracing::info!(node, ?got, ?cur, "a fetched file changed meanwhile: not cached");
                        c.forget(node);
                        Err(EIO)
                    }
                    None => Err(EIO),
                }
            }
            r => {
                if let Ok(n) = r {
                    tracing::warn!(
                        node,
                        got = n,
                        want = f.size,
                        "a fetched file's size differs from the tree"
                    );
                }
                lock(&self.content).abort_fetch(node);
                let _ = std::fs::remove_file(&f.staging);
                r.map_err(|e| e.errno()).and(Err(EIO))
            }
        };
        lock(&self.fetches).remove(&node);
        *lock(&f.done) = Some(res);
        f.cv.notify_all();
    }

    /// After every start: the main transcript into the cache (9.7) unless its entry is current, until it is.
    fn prefetch_main(self: &Arc<Self>) {
        let Some(m) = lock(&self.main).clone() else { return };
        for _ in 0..5 {
            let (node, e) = {
                let mut st = lock(&self.st);
                let Some(e) = st.tree.get(&m).copied() else { return };
                (st.nodes.assign(&m), e)
            };
            lock(&self.content).pin(Some(node));
            if lock(&self.content).valid(node, e.version, e.size).is_some() {
                return;
            }
            if let Got::Done(Err(errno)) = self.fetch(node, &m, e).wait(u64::MAX) {
                tracing::info!(errno, "the main transcript's prefetch failed");
            }
        }
    }

    // ---- the workers ----

    fn spawn_workers(self: &Arc<Self>) {
        for i in 0..self.journal.slots() {
            let c = self.clone();
            std::thread::Builder::new()
                .name(format!("fuse-{i}"))
                .stack_size(256 * 1024)
                .spawn(move || c.worker(i))
                .ok();
        }
    }

    fn point(&self, p: Point, h: &InHeader) {
        let die = self.hooks().fault.as_ref().is_some_and(|f| f(p, h));
        if !die {
            return;
        }
        #[cfg(feature = "fault-injection")]
        {
            tracing::warn!(point = ?p, opcode = proto::opname(h.opcode), unique = h.unique, "fault-injection: SIGKILL");
            // SAFETY: kill(2) of this process.
            #[allow(unsafe_code)]
            unsafe {
                libc::kill(libc::getpid(), libc::SIGKILL);
            }
        }
        self.dead.store(true, Ordering::SeqCst);
        self.dev.close();
        std::panic::panic_any(Died);
    }

    fn worker(self: Arc<Self>, i: usize) {
        SLOT.set(Some(i));
        // the last `unique` this slot held: a header after a short read must name a newer one
        let mut last = 0u64;
        loop {
            if self.stopped.load(Ordering::SeqCst) || self.died() {
                return;
            }
            self.journal.free(i);
            // SAFETY: slot i belongs to this worker alone.
            #[allow(unsafe_code)]
            let slot = unsafe { self.journal.slot(i) };
            let n = match self.dev.read(slot) {
                Ok(n) => n,
                Err(e) => match e.raw_os_error() {
                    Some(libc::EINTR | libc::EAGAIN | libc::ENOENT) => continue,
                    // the connection ended (unmounted, aborted): every worker ends with it
                    Some(libc::ENODEV | libc::EBADF) => {
                        if !self.stopped.load(Ordering::SeqCst) && !self.died() {
                            tracing::info!(slot = i, error = %e, "fuse read ended");
                        }
                        return;
                    }
                    _ => {
                        if self.stopped.load(Ordering::SeqCst) || self.died() {
                            return;
                        }
                        // anything else is not the connection's end: this worker keeps reading, loudly
                        tracing::error!(slot = i, error = %e, "fuse read failed: retrying");
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                },
            };
            if !self.journal.whole_request(i, slot, n) {
                self.short_reads.fetch_add(1, Ordering::SeqCst);
                // the caller must not wait for ever: EIO to the unique a copy of the new header names
                if let Some(h) = self.journal.lost_header(i, slot, last) {
                    last = h.unique;
                    tracing::error!(
                        event = "fuse.short_read",
                        unique = h.unique,
                        op = proto::opname(h.opcode),
                        "answered EIO: no whole request to serve"
                    );
                    self.send(
                        &h,
                        &Outcome {
                            errno: EIO,
                            payload: Vec::new(),
                            effect: false,
                        },
                    );
                } else {
                    // no copy names the request: nothing can be answered (a reply to an older unique would be
                    // refused, or worse, reach another request), so it is logged and counted
                    self.lost_requests.fetch_add(1, Ordering::SeqCst);
                    tracing::error!(
                        event = "fuse.request_lost",
                        slot = i,
                        read = n,
                        "a read left no header of a new request in the slot: its caller cannot be answered"
                    );
                }
                continue;
            }
            let Some(h) = InHeader::parse(slot) else { continue };
            last = h.unique;
            if !self.first_logged.swap(true, Ordering::SeqCst) {
                tracing::info!(unique = h.unique, op = proto::opname(h.opcode), "fuse first request");
            }
            // the test build logs every request, so the rig checks that a `unique` never repeats (22 item 4)
            #[cfg(feature = "fault-injection")]
            tracing::info!(unique = h.unique, op = proto::opname(h.opcode), "fuse request");
            let len = (h.len as usize).min(slot.len());
            let body = &slot[IN_HEADER..len];
            let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
                self.point(Point::ReadReturned, &h);
                self.request(&h, body);
            }));
            if r.is_err() || self.died() {
                // killed at a fault point: the slot stays occupied for the next instance's replay
                return;
            }
            self.journal.free(i);
        }
    }

    /// Writes a reply; the errno of the write (0, or ENOENT when the sentry no longer waits for `unique`).
    fn send(&self, h: &InHeader, o: &Outcome) -> i32 {
        if self.died() {
            std::panic::panic_any(Died);
        }
        let r = proto::reply(h.unique, o.errno, &o.payload);
        let e = match self.dev.write(&r) {
            Ok(()) => 0,
            Err(e) => e.raw_os_error().unwrap_or(EIO),
        };
        if e != 0 && e != ENOENT {
            tracing::warn!(unique = h.unique, errno = e, "fuse reply write failed");
        }
        self.point(Point::ReplyWritten, h);
        e
    }

    fn request(self: &Arc<Self>, h: &InHeader, body: &[u8]) {
        match h.opcode {
            op::FORGET | op::BATCH_FORGET => return,
            op::INTERRUPT => {
                let target = u64_at(body, 0);
                if let Some(c) = lock(&self.ctls).get(&target) {
                    c.interrupt();
                }
                return;
            }
            _ => {}
        }
        self.inflight.fetch_add(1, Ordering::SeqCst);
        struct Done<'a>(&'a Core);
        impl Drop for Done<'_> {
            fn drop(&mut self) {
                self.0.inflight.fetch_sub(1, Ordering::SeqCst);
                let _g = lock(&self.0.idle.0);
                self.0.idle.1.notify_all();
            }
        }
        let _done = Done(self);
        if !proto::mutates(h.opcode) {
            let o = self.serve(h, body);
            self.send(h, &o);
            return;
        }
        // the resend rule of 9.3
        let key: Key = (h.opcode, h.nodeid, sha2::Sha256::digest(body).into());
        let me = Arc::new(Inflight {
            key,
            matched: AtomicBool::new(false),
            done: Mutex::new(None),
            cv: Condvar::new(),
        });
        let original = {
            let mut rs = lock(&self.resend);
            if let Some((k, rec, _, since)) = rs.kept.remove(&h.pid) {
                self.journal.unkeep(k);
                let fresh = since.elapsed() <= self.cfg.kept_ttl;
                if !fresh {
                    tracing::info!(
                        pid = h.pid,
                        op = proto::opname(rec.opcode),
                        "a kept interrupted result expired unused"
                    );
                }
                if fresh && (rec.opcode, rec.nodeid, rec.key) == key {
                    drop(rs);
                    tracing::info!(
                        unique = h.unique,
                        pid = h.pid,
                        op = proto::opname(h.opcode),
                        "resend after a checkpoint answered with the interrupted result"
                    );
                    self.send(
                        h,
                        &Outcome {
                            errno: rec.errno,
                            payload: rec.payload,
                            effect: true,
                        },
                    );
                    return;
                }
            }
            let list = rs.inflight.entry(h.pid).or_default();
            let orig = list.iter().find(|i| i.key == key).cloned();
            list.push(me.clone());
            orig
        };
        let mut outcome = None;
        if let Some(orig) = original {
            tracing::info!(
                unique = h.unique,
                pid = h.pid,
                op = proto::opname(h.opcode),
                "resend of a request still in flight: waiting for its result"
            );
            let mut g = lock(&orig.done);
            while g.is_none() {
                g = orig.cv.wait(g).unwrap_or_else(PoisonError::into_inner);
            }
            let o = g.clone();
            drop(g);
            if let Some(o) = o.filter(|o| o.effect) {
                // taken: the original's refused reply must not be kept as well
                let mut rs = lock(&self.resend);
                orig.matched.store(true, Ordering::SeqCst);
                if rs
                    .kept
                    .get(&h.pid)
                    .is_some_and(|(_, r, _, _)| (r.opcode, r.nodeid, r.key) == key)
                    && let Some((k, _, _, _)) = rs.kept.remove(&h.pid)
                {
                    self.journal.unkeep(k);
                }
                outcome = Some(o);
            }
        }
        let o = match outcome {
            Some(o) => o,
            None => self.serve(h, body),
        };
        {
            *lock(&me.done) = Some(o.clone());
            me.cv.notify_all();
        }
        // Under the resend lock from leaving the in-flight list to keeping a refused reply: the thread's next
        // request (sent once this reply arrived, and possibly identical: a second `mkdir` of a held lock) must never
        // find this one still listed and be taken for its resend, and a real resend finds it either in flight or
        // kept.
        let mut rs = lock(&self.resend);
        if let Some(list) = rs.inflight.get_mut(&h.pid) {
            list.retain(|i| !Arc::ptr_eq(i, &me));
            if list.is_empty() {
                rs.inflight.remove(&h.pid);
            }
        }
        let werr = self.send(h, &o);
        if werr == ENOENT && o.effect && !me.matched.load(Ordering::SeqCst) {
            self.keep_locked(&mut rs, h, key, &o);
        }
    }

    fn keep_locked(&self, rs: &mut Resend, h: &InHeader, key: Key, o: &Outcome) {
        if let Some((k, _, _, _)) = rs.kept.remove(&h.pid) {
            self.journal.unkeep(k);
        }
        if rs.kept.len() >= KEPT_SLOTS
            && let Some(oldest) = rs.kept.iter().min_by_key(|(_, v)| v.2).map(|(p, _)| *p)
            && let Some((k, _, _, _)) = rs.kept.remove(&oldest)
        {
            self.journal.unkeep(k);
        }
        let used: Vec<usize> = rs.kept.values().map(|v| v.0).collect();
        let Some(k) = (0..KEPT_SLOTS).find(|k| !used.contains(k)) else {
            return;
        };
        let rec = KeptRecord {
            pid: h.pid,
            opcode: key.0,
            nodeid: key.1,
            key: key.2,
            errno: o.errno,
            payload: o.payload.clone(),
        };
        self.journal.keep(k, &rec);
        rs.order += 1;
        let ord = rs.order;
        rs.kept.insert(h.pid, (k, rec, ord, std::time::Instant::now()));
        tracing::info!(
            unique = h.unique,
            pid = h.pid,
            op = proto::opname(h.opcode),
            "reply refused (ENOENT): result kept for the thread's resend"
        );
    }

    fn restore_kept(&self) {
        let mut rs = lock(&self.resend);
        for (k, rec) in self.journal.kept() {
            rs.order += 1;
            let ord = rs.order;
            if let Some((old, _, _, _)) = rs.kept.insert(rec.pid, (k, rec, ord, std::time::Instant::now())) {
                self.journal.unkeep(old);
            }
        }
    }

    /// 9.9: every occupied slot in `unique` order, re-executed with its derived `opId`s, the reply written with the
    /// original `unique` (ENOENT ignored: the reply was delivered before the kill, or the caller is gone).
    fn replay(self: &Arc<Self>) {
        let pending = self.journal.occupied();
        for (unique, i) in &pending {
            // SAFETY: no worker runs yet.
            #[allow(unsafe_code)]
            let slot = unsafe { self.journal.slot(*i) };
            let Some(h) = InHeader::parse(slot) else {
                self.journal.free(*i);
                continue;
            };
            let len = (h.len as usize).min(slot.len());
            let body = slot[IN_HEADER.min(len)..len].to_vec();
            if matches!(h.opcode, op::FORGET | op::BATCH_FORGET | op::INTERRUPT) {
                self.journal.free(*i);
                continue;
            }
            SLOT.set(Some(*i));
            let o = self.serve_as(&h, &body, true);
            SLOT.set(None);
            let e = self.send(&h, &o);
            tracing::info!(unique, op = proto::opname(h.opcode), reply_errno = e, "journal replay");
            self.journal.free(*i);
        }
    }

    // ---- the operations ----

    fn serve(self: &Arc<Self>, h: &InHeader, body: &[u8]) -> Outcome {
        self.serve_as(h, body, false)
    }

    fn serve_as(self: &Arc<Self>, h: &InHeader, body: &[u8], replay: bool) -> Outcome {
        let ctl = Arc::new(OpCtl::new(replay));
        lock(&self.ctls).insert(h.unique, ctl.clone());
        let mut effect = false;
        let r = self.dispatch(h, body, &ctl, &mut effect);
        lock(&self.ctls).remove(&h.unique);
        Outcome::of(r, effect)
    }

    fn path_of(&self, node: u64) -> Result<(String, bool), i32> {
        let st = lock(&self.st);
        st.nodes.get(node).map(|(p, g)| (p.to_owned(), g)).ok_or(ENOENT)
    }

    /// Whether magpie-scope requests are served: a lease, and not between a lost lease and the reload of the tree
    /// under the new one.
    fn is_leased(&self) -> bool {
        self.remote.lease().is_some() && !self.relocking.load(Ordering::SeqCst)
    }

    fn leased(&self) -> Result<(), i32> {
        if self.is_leased() { Ok(()) } else { Err(EIO) }
    }

    fn root_attr(&self) -> Attr {
        Attr {
            ino: ROOT,
            size: 0,
            mtime_ns: self.started_ns,
            ctime_ns: self.started_ns,
            perm: 0o700,
            kind: FileType::Dir,
        }
    }

    fn magpie_attr(ino: u64, e: &Entry) -> Attr {
        Attr {
            ino,
            size: e.size,
            mtime_ns: e.mtime_ns,
            ctime_ns: e.ctime_ns,
            perm: e.mode & 0o7777,
            kind: if e.is_dir() { FileType::Dir } else { FileType::File },
        }
    }

    /// The attributes of a node (magpie: the tree; local: lstat; root: fixed).
    fn attr(&self, ino: u64, path: &str, gone: bool, handle: Option<u64>) -> Result<Attr, i32> {
        match scope::scope_of(path) {
            Scope::Top => Ok(self.root_attr()),
            Scope::Refused => Err(ENOENT),
            // a home is its backing directory, which the mount made
            Scope::Local | Scope::Root => self.local.getattr(ino, path, handle),
            Scope::Magpie => {
                self.leased()?;
                let st = lock(&self.st);
                let e = if gone { st.gone.get(&ino) } else { st.tree.get(path) };
                e.map(|e| Self::magpie_attr(ino, e)).ok_or(ENOENT)
            }
        }
    }

    fn entry(&self, a: &Attr) -> Vec<u8> {
        proto::entry_out(a, self.cfg.attr_valid)
    }

    #[allow(clippy::too_many_lines)] // one arm per opcode, as 9.3's table lists them
    fn dispatch(self: &Arc<Self>, h: &InHeader, b: &[u8], ctl: &OpCtl, effect: &mut bool) -> Result<Vec<u8>, i32> {
        match h.opcode {
            op::INIT => Ok(proto::init_out(b, self.cfg.max_write, self.cfg.max_pages)),
            op::DESTROY => Ok(Vec::new()),
            op::LOOKUP => {
                let (name, _) = cstr(b).ok_or(EINVAL)?;
                let (parent, _) = self.path_of(h.nodeid)?;
                let p = scope::join(&parent, name)?;
                self.unclassified.note(&p);
                let a = match scope::scope_of(&p) {
                    Scope::Refused => return Err(ENOENT),
                    Scope::Magpie => {
                        self.leased()?;
                        let mut st = lock(&self.st);
                        let e = *st.tree.get(&p).ok_or(ENOENT)?;
                        let id = st.nodes.assign(&p);
                        Self::magpie_attr(id, &e)
                    }
                    Scope::Local | Scope::Root => {
                        let mut a = self.local.getattr(0, &p, None)?;
                        a.ino = lock(&self.st).nodes.assign(&p);
                        a
                    }
                    Scope::Top => return Err(EINVAL), // a joined path is never the mount itself
                };
                Ok(self.entry(&a))
            }
            op::GETATTR => {
                let handle = (u32_at(b, 0) & 1 != 0).then(|| u64_at(b, 8));
                let (p, gone) = self.path_of(h.nodeid)?;
                let a = self.attr(h.nodeid, &p, gone, handle)?;
                Ok(proto::attr_out(&a, self.cfg.attr_valid))
            }
            op::SETATTR => self.setattr(h, b, ctl, effect),
            op::READLINK => {
                let (p, _) = self.path_of(h.nodeid)?;
                match scope::scope_of(&p) {
                    Scope::Local => self.local.readlink(&p),
                    _ => Err(EINVAL),
                }
            }
            op::SYMLINK => {
                let (name, rest) = cstr(b).ok_or(EINVAL)?;
                let (target, _) = cstr(rest).ok_or(EINVAL)?;
                let (parent, _) = self.path_of(h.nodeid)?;
                let p = scope::join(&parent, name)?;
                match scope::scope_of(&p) {
                    Scope::Local => {
                        *effect = true;
                        match self.local.symlink(target, &p) {
                            Ok(()) => self.mark_done(h),
                            Err(e)
                                if e == EEXIST
                                    && ctl.replay
                                    && self.done_before(h)
                                    && self.local.readlink(&p).ok().as_deref() == Some(target) =>
                            {
                                replayed_as_done(h, &p);
                            }
                            Err(e) => return Err(e),
                        }
                        self.answered(h);
                        let mut a = self.local.getattr(0, &p, None)?;
                        a.ino = lock(&self.st).nodes.assign(&p);
                        Ok(self.entry(&a))
                    }
                    Scope::Refused => Err(EACCES),
                    _ => Err(EPERM),
                }
            }
            op::MKNOD | op::LINK => self.unanswered_op(h.opcode, EPERM),
            op::MKDIR => self.mkdir(h, b, ctl, effect),
            op::UNLINK | op::RMDIR => self.remove(h, b, ctl, effect),
            op::RENAME | op::RENAME2 => self.rename(h, b, ctl, effect),
            op::OPEN => self.open(h, b, ctl, effect),
            op::CREATE => self.create(h, b, ctl, effect),
            op::READ => self.read(h, b),
            op::WRITE => self.write(h, b, ctl, effect),
            op::STATFS => {
                let st = lock(&self.st);
                let q = st.tree.quota_bytes.max(st.tree.cc_bytes);
                let files = 1_000_000u64;
                Ok(proto::statfs_out(
                    q / 4096,
                    (q - st.tree.cc_bytes) / 4096,
                    files,
                    files.saturating_sub(st.tree.files.len() as u64),
                ))
            }
            op::RELEASE => {
                let handle = u64_at(b, 0);
                if let Ok((p, _)) = self.path_of(fh::node(handle))
                    && scope::scope_of(&p) == Scope::Local
                {
                    self.local.release(handle);
                }
                Ok(Vec::new())
            }
            op::FLUSH | op::FSYNC | op::FSYNCDIR => {
                let handle = u64_at(b, 0);
                let (p, _) = self.path_of(h.nodeid)?;
                if h.opcode == op::FSYNC && scope::scope_of(&p) == Scope::Local {
                    self.local.fsync(handle, &p)?;
                }
                Ok(Vec::new())
            }
            op::OPENDIR => {
                let (p, gone) = self.path_of(h.nodeid)?;
                let a = self.attr(h.nodeid, &p, gone, None)?;
                if a.kind != FileType::Dir {
                    return Err(ENOTDIR);
                }
                Ok(proto::open_out(0))
            }
            op::READDIR => self.readdir(h, b),
            op::RELEASEDIR | op::ACCESS => Ok(Vec::new()),
            op::GETXATTR => Err(ENODATA),
            op::LISTXATTR => {
                if u32_at(b, 0) == 0 {
                    Ok(vec![0u8; 8])
                } else {
                    Ok(Vec::new())
                }
            }
            op::SETXATTR | op::REMOVEXATTR => self.unanswered_op(h.opcode, ENOTSUP),
            op::FALLOCATE => self.unanswered_op(h.opcode, EOPNOTSUPP),
            _ => self.unanswered_op(h.opcode, ENOSYS),
        }
    }

    /// Refuses an opcode the mount does not serve, logging it once (`claude-code-gate` reads the log).
    fn unanswered_op(&self, opcode: u32, errno: i32) -> Result<Vec<u8>, i32> {
        self.unanswered.note(opcode, proto::opname(opcode), errno);
        Err(errno)
    }

    fn local_kind(&self, p: &str) -> Option<FileType> {
        self.local.getattr(0, p, None).ok().map(|a| a.kind)
    }

    fn op_id(&self, h: &InHeader, step: u32) -> Uuid {
        crate::magpie::op_id(self.boot, h.unique, step)
    }

    /// One state-changing magpie call for request `h`, with the fault points and the lease and stuck rules.
    fn mcall<T, F, Fut>(
        self: &Arc<Self>,
        h: &InHeader,
        ctl: &OpCtl,
        effect: &mut bool,
        what: &str,
        f: F,
    ) -> Result<T, CallError>
    where
        F: Fn(super::remote::Calls) -> Fut,
        Fut: std::future::Future<Output = Result<T, magpie_api::client::ClientError>>,
    {
        let sent = || self.point(Point::CallSent, h);
        let r = self.rt.block_on(self.remote.call(ctl, what, f, &sent));
        if ctl.executed() {
            *effect = true;
        }
        self.after_call(what, &r);
        r
    }

    /// What every leased magpie call's outcome sets off, whoever made it (a request, or a repair's writes): the
    /// quota flag, the lease loop on `lease_lost` (9.5 item 7), the stuck-op report (item 4).
    fn after_call<T>(self: &Arc<Self>, what: &str, r: &Result<T, CallError>) {
        match r {
            Ok(_) => self.quota_exceeded.store(false, Ordering::SeqCst),
            Err(CallError::Errno(e)) if *e == libc::EDQUOT => {
                tracing::warn!(what, "magpie quota exceeded");
                self.quota_exceeded.store(true, Ordering::SeqCst);
            }
            Err(CallError::LeaseLost) => self.on_lease_lost(),
            Err(CallError::Stuck(msg)) => {
                self.stuck.fetch_add(1, Ordering::SeqCst);
                self.seen.1.notify_all();
                (self.hooks().write_stuck)(StuckOp {
                    what: what.to_owned(),
                    message: msg.clone(),
                });
            }
            _ => {}
        }
    }

    fn answered(&self, h: &InHeader) {
        self.point(Point::CallAnswered, h);
    }

    /// Marks this request's local-scope change as made (its slot's done mark, 9.9).
    fn mark_done(&self, h: &InHeader) {
        if let Some(i) = SLOT.get() {
            self.journal.mark_done(i, h.unique);
        }
    }

    /// Whether the killed instance made this replayed request's local-scope change (its slot's done mark).
    fn done_before(&self, h: &InHeader) -> bool {
        SLOT.get().is_some_and(|i| self.journal.marked_done(i, h.unique))
    }

    fn magpie_entry(&self, path: &str) -> Result<Entry, i32> {
        lock(&self.st).tree.get(path).copied().ok_or(ENOENT)
    }

    fn setattr(self: &Arc<Self>, h: &InHeader, b: &[u8], ctl: &OpCtl, effect: &mut bool) -> Result<Vec<u8>, i32> {
        let valid = u32_at(b, 0);
        let (p, gone) = self.path_of(h.nodeid)?;
        let now = now_ns();
        #[allow(clippy::cast_possible_wrap)] // seconds since the epoch fit
        let ts = |s: u64, ns: u32| (s as i64).saturating_mul(1_000_000_000) + i64::from(ns);
        let mtime = if valid & fattr::MTIME_NOW != 0 {
            Some(now)
        } else if valid & fattr::MTIME != 0 {
            Some(ts(u64_at(b, 40), u32_at(b, 60)))
        } else {
            None
        };
        let atime = if valid & fattr::ATIME_NOW != 0 {
            Some(now)
        } else if valid & fattr::ATIME != 0 {
            Some(ts(u64_at(b, 32), u32_at(b, 56)))
        } else {
            None
        };
        let set = SetAttr {
            handle: (valid & fattr::FH != 0).then(|| u64_at(b, 8)),
            size: (valid & fattr::SIZE != 0).then(|| u64_at(b, 16)),
            mode: (valid & fattr::MODE != 0).then(|| u32_at(b, 68) & 0o7777),
            uid: (valid & fattr::UID != 0).then(|| u32_at(b, 76)),
            gid: (valid & fattr::GID != 0).then(|| u32_at(b, 80)),
            atime_ns: atime,
            mtime_ns: mtime,
        };
        if set.uid.is_some_and(|u| u != proto::OWNER) || set.gid.is_some_and(|g| g != proto::OWNER) {
            return Err(EPERM);
        }
        match scope::scope_of(&p) {
            Scope::Top => return Ok(proto::attr_out(&self.root_attr(), self.cfg.attr_valid)),
            // a home keeps the attributes the mount gave it
            Scope::Root => {
                let a = self.local.getattr(h.nodeid, &p, None)?;
                return Ok(proto::attr_out(&a, self.cfg.attr_valid));
            }
            Scope::Refused => return Err(ENOENT),
            Scope::Local => {
                *effect = true;
                self.local.setattr(&p, &set)?;
            }
            Scope::Magpie => {
                self.leased()?;
                if gone {
                    return Err(ENOENT);
                }
                if let Some(size) = set.size {
                    let e = self.magpie_entry(&p)?;
                    if e.is_dir() {
                        return Err(libc::EISDIR);
                    }
                    let req = CcTruncate {
                        path: p.clone(),
                        size,
                        mtime_ns: now,
                        op_id: self.op_id(h, 0),
                    };
                    let sv = self.with_repair(h, &p, |s| {
                        s.mcall(h, ctl, effect, "cc/truncate", |c| {
                            let r = req.clone();
                            async move { c.sb().cc_truncate(&r).await }
                        })
                    })?;
                    self.answered(h);
                    let node = h.nodeid;
                    let mut st = lock(&self.st);
                    let mut ne = e;
                    ne.size = sv.size;
                    ne.version = sv.version;
                    ne.mtime_ns = now;
                    ne.ctime_ns = now;
                    st.tree.insert(&p, ne);
                    lock(&self.content).truncated(node, (e.version, e.size), sv.version, sv.size);
                }
                if set.mode.is_some() || set.mtime_ns.is_some() {
                    let e = self.magpie_entry(&p)?;
                    let req = CcSetattr {
                        path: p.clone(),
                        mode: set.mode,
                        mtime_ns: set.mtime_ns,
                        op_id: self.op_id(h, 1),
                    };
                    let v = self
                        .mcall(h, ctl, effect, "cc/setattr", |c| {
                            let r = req.clone();
                            async move { c.sb().cc_setattr(&r).await }
                        })
                        .map_err(|e| e.errno())?;
                    self.answered(h);
                    let mut ne = e;
                    if let Some(m) = set.mode {
                        ne.mode = m;
                    }
                    if let Some(t) = set.mtime_ns {
                        ne.mtime_ns = t;
                    }
                    ne.ctime_ns = now;
                    ne.version = v.version;
                    lock(&self.st).tree.insert(&p, ne);
                    if !e.is_dir() {
                        lock(&self.content).reversion(h.nodeid, (e.version, e.size), v.version);
                    }
                }
            }
        }
        let (p, gone) = self.path_of(h.nodeid)?;
        let a = self.attr(h.nodeid, &p, gone, set.handle)?;
        Ok(proto::attr_out(&a, self.cfg.attr_valid))
    }

    /// Runs a write or truncate; on `offset_mismatch`/`offset_beyond_eof` repairs from the cache (9.5 item 6) and
    /// runs it once more. A fork (the cache disagrees with magpie) is EIO, and the agent is stopped by the hook.
    fn with_repair<T>(
        self: &Arc<Self>,
        _h: &InHeader,
        path: &str,
        mut f: impl FnMut(&Arc<Self>) -> Result<T, CallError>,
    ) -> Result<T, i32> {
        match f(self) {
            Err(CallError::Repair) => {}
            r => return r.map_err(|e| e.errno()),
        }
        tracing::warn!(path, "offset refused by magpie: repairing from the cache");
        if !self.repair_file(path) {
            return Err(EIO);
        }
        f(self).map_err(|e| e.errno())
    }

    fn repair_file(self: &Arc<Self>, path: &str) -> bool {
        let ctl = OpCtl::default();
        let t = self.rt.block_on(self.remote.read(
            &ctl,
            "cc/tree",
            |c| async move { c.client.sandbox(None).cc_tree().await },
            &|| {},
        ));
        let Ok(t) = t else { return false };
        let theirs = Tree::from_magpie(&t);
        let Some(e) = theirs.get(path).copied() else {
            return false;
        };
        let (node, ours) = {
            let st = lock(&self.st);
            let Some(n) = st.nodes.id_of(path) else { return false };
            (n, st.tree.get(path).copied())
        };
        let cached = ours.and_then(|o| lock(&self.content).valid(node, o.version, o.size).map(|f| (f, o)));
        let Some((file, o)) = cached else {
            tracing::error!(
                path,
                magpie_size = e.size,
                "offset refused and no cached copy to repair from"
            );
            (self.hooks().forked)(format!("{path}: offset refused, nothing cached"));
            return false;
        };
        match self.resend_suffix(path, &file, e, o.size) {
            Resent::Done(v) => {
                lock(&self.content).reversion(node, (o.version, o.size), v);
                true
            }
            Resent::NotAhead => {
                (self.hooks().forked)(format!("{path}: magpie has {} bytes, the cache {}", e.size, o.size));
                false
            }
            Resent::Forked | Resent::Failed { .. } => false,
        }
    }

    fn open(self: &Arc<Self>, h: &InHeader, b: &[u8], ctl: &OpCtl, effect: &mut bool) -> Result<Vec<u8>, i32> {
        #[allow(clippy::cast_possible_wrap)] // open(2) flags
        let flags = u32_at(b, 0) as i32;
        let (p, gone) = self.path_of(h.nodeid)?;
        match scope::scope_of(&p) {
            Scope::Top | Scope::Root => Ok(proto::open_out(fh::of(h.nodeid, flags))),
            Scope::Refused => Err(ENOENT),
            Scope::Local => {
                if flags & libc::O_TRUNC != 0 {
                    *effect = true;
                }
                Ok(proto::open_out(self.local.open_file(h.nodeid, &p, flags)?))
            }
            Scope::Magpie => {
                self.leased()?;
                if gone {
                    if lock(&self.st).gone.contains_key(&h.nodeid) {
                        return Ok(proto::open_out(fh::of(h.nodeid, flags)));
                    }
                    return Err(ENOENT);
                }
                let e = self.magpie_entry(&p)?;
                if flags & libc::O_TRUNC != 0 && !e.is_dir() && e.size > 0 {
                    let now = now_ns();
                    let req = CcTruncate {
                        path: p.clone(),
                        size: 0,
                        mtime_ns: now,
                        op_id: self.op_id(h, 0),
                    };
                    let sv = self
                        .mcall(h, ctl, effect, "cc/truncate", |c| {
                            let r = req.clone();
                            async move { c.sb().cc_truncate(&r).await }
                        })
                        .map_err(|e| e.errno())?;
                    self.answered(h);
                    let mut ne = e;
                    ne.size = 0;
                    ne.version = sv.version;
                    ne.mtime_ns = now;
                    ne.ctime_ns = now;
                    lock(&self.st).tree.insert(&p, ne);
                    lock(&self.content).truncated(h.nodeid, (e.version, e.size), sv.version, 0);
                }
                Ok(proto::open_out(fh::of(h.nodeid, flags)))
            }
        }
    }

    fn create(self: &Arc<Self>, h: &InHeader, b: &[u8], ctl: &OpCtl, effect: &mut bool) -> Result<Vec<u8>, i32> {
        #[allow(clippy::cast_possible_wrap)] // open(2) flags
        let flags = u32_at(b, 0) as i32;
        let mode = u32_at(b, 4) & 0o7777 & !u32_at(b, 8);
        let (name, _) = cstr(b.get(16..).ok_or(EINVAL)?).ok_or(EINVAL)?;
        let (parent, _) = self.path_of(h.nodeid)?;
        let p = scope::join(&parent, name)?;
        self.unclassified.note(&p);
        match scope::scope_of(&p) {
            Scope::Refused => Err(EACCES),
            Scope::Top => Err(EINVAL),
            Scope::Local | Scope::Root => {
                *effect = true;
                let handle = match self.local.create(|| lock(&self.st).nodes.assign(&p), &p, flags, mode) {
                    Ok(handle) => {
                        self.mark_done(h);
                        handle
                    }
                    Err(e)
                        if e == EEXIST
                            && ctl.replay
                            && self.done_before(h)
                            && self.local_kind(&p) == Some(FileType::File) =>
                    {
                        // the create took effect before the kill: open what it made (no O_EXCL, no O_TRUNC)
                        replayed_as_done(h, &p);
                        let f = flags & !(libc::O_EXCL | libc::O_TRUNC);
                        self.local.create(|| lock(&self.st).nodes.assign(&p), &p, f, mode)?
                    }
                    Err(e) => return Err(e),
                };
                self.answered(h);
                let a = self.local.getattr(fh::node(handle), &p, Some(handle))?;
                let mut o = self.entry(&a);
                o.extend_from_slice(&proto::open_out(handle));
                Ok(o)
            }
            Scope::Magpie => {
                self.leased()?;
                let existing = {
                    let st = lock(&self.st);
                    if !st.tree.is_dir(&parent) {
                        return Err(if st.tree.get(&parent).is_some() {
                            ENOTDIR
                        } else {
                            ENOENT
                        });
                    }
                    st.tree.get(&p).copied()
                };
                if let Some(e) = existing.filter(|_| !ctl.replay) {
                    // answered from the tree where definitive (the supervisor is the only writer)
                    if flags & libc::O_EXCL != 0 {
                        return Err(EEXIST);
                    }
                    if e.is_dir() {
                        return Err(libc::EISDIR);
                    }
                    let node = lock(&self.st).nodes.assign(&p);
                    let hh = InHeader { nodeid: node, ..*h };
                    let mut ob = flags.to_le_bytes().to_vec();
                    ob.extend_from_slice(&[0; 4]);
                    let o = self.open(&hh, &ob, ctl, effect)?;
                    let e = self.magpie_entry(&p)?;
                    let mut out = self.entry(&Self::magpie_attr(node, &e));
                    out.extend_from_slice(&o);
                    return Ok(out);
                }
                let now = now_ns();
                let q = CcWriteQuery {
                    path: p.clone(),
                    offset: 0,
                    mtime_ns: now,
                    op_id: self.op_id(h, 0),
                    create: true,
                    mode,
                    turn_id: *lock(&self.turn),
                    append: false,
                };
                let sv = self
                    .mcall(h, ctl, effect, "cc/write (create)", |c| {
                        let q = q.clone();
                        async move { c.sb().cc_write(&q, Bytes::new()).await }
                    })
                    .map_err(|e| e.errno())?;
                self.answered(h);
                let e = Entry {
                    kind: cache::kind_of(&p),
                    size: sv.size,
                    mtime_ns: now,
                    ctime_ns: now,
                    mode,
                    version: sv.version,
                };
                let node = {
                    let mut st = lock(&self.st);
                    st.tree.insert(&p, e);
                    st.nodes.assign(&p)
                };
                if sv.size == 0 {
                    lock(&self.content).created(node, sv.version);
                }
                if lock(&self.main).as_deref() == Some(p.as_str()) {
                    lock(&self.content).pin(Some(node));
                }
                let mut out = self.entry(&Self::magpie_attr(node, &e));
                out.extend_from_slice(&proto::open_out(fh::of(node, flags)));
                Ok(out)
            }
        }
    }

    fn read(self: &Arc<Self>, h: &InHeader, b: &[u8]) -> Result<Vec<u8>, i32> {
        let handle = u64_at(b, 0);
        let off = u64_at(b, 8);
        let size = u32_at(b, 16);
        let (p, gone) = self.path_of(h.nodeid)?;
        match scope::scope_of(&p) {
            Scope::Local => self.local.read(handle, &p, off, size),
            Scope::Top | Scope::Root => Err(libc::EISDIR),
            Scope::Refused => Err(ENOENT),
            Scope::Magpie => {
                self.leased()?;
                let node = h.nodeid;
                if gone {
                    let (f, _) = lock(&self.content).any(node).ok_or(ENOENT)?;
                    return cache::read_at(&f, off, size as usize).map_err(|_| EIO);
                }
                let e = self.magpie_entry(&p)?;
                if e.is_dir() {
                    return Err(libc::EISDIR);
                }
                if off >= e.size {
                    return Ok(Vec::new());
                }
                let end = (off + u64::from(size)).min(e.size);
                #[allow(clippy::cast_possible_truncation)] // at most `size`
                let want = (end - off) as usize;
                // the range from magpie: when the cache cannot hold the file (disk full) or the fetch could not
                // become a cache entry
                let direct = || self.fetch_range(&OpCtl::default(), &p, off, end - off);
                if let Some(f) = lock(&self.content).valid(node, e.version, e.size) {
                    return cache::read_at(&f, off, want).or_else(|_| direct());
                }
                if e.size > magpie_api::sandbox::CC_FILE_MAX_BYTES {
                    return Err(EFBIG);
                }
                let fetch = self.fetch(node, &p, e);
                // an older fetch (the tree moved since) ends at the newer version: wait for its end
                let upto = if fetch.version == e.version { end } else { u64::MAX };
                match fetch.wait(upto) {
                    Got::Range(staging) => match cache::read_at(&staging, off, want) {
                        Ok(b) if b.len() == want => Ok(b),
                        _ => match fetch.wait(u64::MAX) {
                            // (the staging file just became the cache entry)
                            Got::Done(Ok(f)) => cache::read_at(&f, off, want).or_else(|_| direct()),
                            _ => direct(),
                        },
                    },
                    Got::Done(Ok(f)) => cache::read_at(&f, off, want).or_else(|_| direct()),
                    Got::Done(Err(err)) if err == EIO => direct(),
                    Got::Done(Err(err)) => Err(err),
                }
            }
        }
    }

    fn write(self: &Arc<Self>, h: &InHeader, b: &[u8], ctl: &OpCtl, effect: &mut bool) -> Result<Vec<u8>, i32> {
        let handle = u64_at(b, 0);
        let off = u64_at(b, 8);
        let size = u32_at(b, 16) as usize;
        let data = b.get(40..40 + size).ok_or(EINVAL)?;
        let (p, gone) = self.path_of(h.nodeid)?;
        match scope::scope_of(&p) {
            Scope::Local => {
                *effect = true;
                Ok(proto::write_out(self.local.write(handle, &p, off, data)?))
            }
            Scope::Top | Scope::Root => Err(libc::EISDIR),
            Scope::Refused => Err(ENOENT),
            Scope::Magpie => {
                self.leased()?;
                if gone {
                    return Err(ENOENT);
                }
                let e = self.magpie_entry(&p)?;
                let now = now_ns();
                let q = CcWriteQuery {
                    path: p.clone(),
                    offset: off,
                    mtime_ns: now,
                    op_id: self.op_id(h, 0),
                    create: false,
                    mode: e.mode,
                    turn_id: *lock(&self.turn),
                    // an O_APPEND handle (Claude Code's transcript appends): `offset` is the size the sentry expects,
                    // and magpie answers `offset_mismatch` to any other size instead of overwriting inside the file
                    // (magpie 6.10), which sends it to the repair of 9.5 item 6
                    append: handle & fh::APPEND != 0,
                };
                let bytes = Bytes::copy_from_slice(data);
                let sv = self.with_repair(h, &p, |s| {
                    s.mcall(h, ctl, effect, "cc/write", |c| {
                        let q = q.clone();
                        let d = bytes.clone();
                        async move { c.sb().cc_write(&q, d).await }
                    })
                })?;
                self.answered(h);
                let old = self.magpie_entry(&p).unwrap_or(e);
                let mut ne = old;
                ne.size = sv.size;
                ne.version = sv.version;
                ne.mtime_ns = now;
                ne.ctime_ns = now;
                lock(&self.st).tree.insert(&p, ne);
                lock(&self.content).wrote(h.nodeid, (old.version, old.size), sv.version, off, data, sv.size);
                if std::path::Path::new(&p).extension().is_some_and(|x| x == "jsonl") {
                    lock(&self.seen.0).wrote(&p, off, data);
                    self.seen.1.notify_all();
                }
                #[allow(clippy::cast_possible_truncation)] // at most 1 MiB
                Ok(proto::write_out(size as u32))
            }
        }
    }

    fn mkdir(self: &Arc<Self>, h: &InHeader, b: &[u8], ctl: &OpCtl, effect: &mut bool) -> Result<Vec<u8>, i32> {
        let mode = u32_at(b, 0) & 0o7777 & !u32_at(b, 4);
        let (name, _) = cstr(b.get(8..).ok_or(EINVAL)?).ok_or(EINVAL)?;
        let (parent, _) = self.path_of(h.nodeid)?;
        let p = scope::join(&parent, name)?;
        self.unclassified.note(&p);
        match scope::scope_of(&p) {
            Scope::Refused => Err(EACCES),
            Scope::Top => Err(EINVAL),
            Scope::Local | Scope::Root => {
                *effect = true;
                match self.local.mkdir(&p, mode) {
                    Ok(()) => self.mark_done(h),
                    Err(e)
                        if e == EEXIST
                            && ctl.replay
                            && self.done_before(h)
                            && self.local_kind(&p) == Some(FileType::Dir) =>
                    {
                        replayed_as_done(h, &p);
                    }
                    Err(e) => return Err(e),
                }
                self.answered(h);
                let mut a = self.local.getattr(0, &p, None)?;
                a.ino = lock(&self.st).nodes.assign(&p);
                Ok(self.entry(&a))
            }
            Scope::Magpie => {
                self.leased()?;
                if !ctl.replay {
                    let st = lock(&self.st);
                    if st.tree.get(&p).is_some() {
                        return Err(EEXIST);
                    }
                    if !st.tree.is_dir(&parent) {
                        return Err(if st.tree.get(&parent).is_some() {
                            ENOTDIR
                        } else {
                            ENOENT
                        });
                    }
                }
                let now = now_ns();
                let req = CcMkdir {
                    path: p.clone(),
                    mode,
                    mtime_ns: now,
                    op_id: self.op_id(h, 0),
                };
                self.mcall(h, ctl, effect, "cc/mkdir", |c| {
                    let r = req.clone();
                    async move { c.sb().cc_mkdir(&r).await }
                })
                .map_err(|e| e.errno())?;
                self.answered(h);
                let e = Entry {
                    kind: CcKind::Dir,
                    size: 0,
                    mtime_ns: now,
                    ctime_ns: now,
                    mode,
                    version: 1,
                };
                let mut st = lock(&self.st);
                st.tree.insert(&p, e);
                let id = st.nodes.assign(&p);
                Ok(self.entry(&Self::magpie_attr(id, &e)))
            }
        }
    }

    fn remove(self: &Arc<Self>, h: &InHeader, b: &[u8], ctl: &OpCtl, effect: &mut bool) -> Result<Vec<u8>, i32> {
        let rmdir = h.opcode == op::RMDIR;
        let (name, _) = cstr(b).ok_or(EINVAL)?;
        let (parent, _) = self.path_of(h.nodeid)?;
        let p = scope::join(&parent, name)?;
        match scope::scope_of(&p) {
            Scope::Refused => Err(ENOENT),
            // the homes belong to the mount
            Scope::Top | Scope::Root => Err(libc::EBUSY),
            Scope::Local => {
                *effect = true;
                let r = if rmdir {
                    self.local.rmdir(&p)
                } else {
                    self.local.unlink(&p)
                };
                match r {
                    Ok(()) => self.mark_done(h),
                    Err(e) if e == ENOENT && ctl.replay && self.done_before(h) => replayed_as_done(h, &p),
                    Err(e) => return Err(e),
                }
                self.answered(h);
                lock(&self.st).nodes.drop_path(&p);
                Ok(Vec::new())
            }
            Scope::Magpie => {
                self.leased()?;
                if !ctl.replay {
                    let st = lock(&self.st);
                    let e = st.tree.get(&p).ok_or(ENOENT)?;
                    if rmdir && !e.is_dir() {
                        return Err(ENOTDIR);
                    }
                    if !rmdir && e.is_dir() {
                        return Err(libc::EISDIR);
                    }
                    if rmdir && st.tree.has_children(&p) {
                        return Err(ENOTEMPTY);
                    }
                }
                let req = CcPathOp {
                    path: p.clone(),
                    op_id: self.op_id(h, 0),
                };
                self.mcall(h, ctl, effect, if rmdir { "cc/rmdir" } else { "cc/unlink" }, |c| {
                    let r = req.clone();
                    async move {
                        if rmdir {
                            c.sb().cc_rmdir(&r).await
                        } else {
                            c.sb().cc_unlink(&r).await
                        }
                    }
                })
                .map_err(|e| e.errno())?;
                self.answered(h);
                let mut st = lock(&self.st);
                let e = st.tree.remove(&p);
                let id = st.nodes.id_of(&p);
                st.nodes.drop_path(&p);
                if let (Some(id), Some(e)) = (id, e) {
                    st.gone.insert(id, e);
                }
                Ok(Vec::new())
            }
        }
    }

    fn rename(self: &Arc<Self>, h: &InHeader, b: &[u8], ctl: &OpCtl, effect: &mut bool) -> Result<Vec<u8>, i32> {
        let newdir = u64_at(b, 0);
        let rest = if h.opcode == op::RENAME2 {
            if u32_at(b, 8) != 0 {
                return Err(EINVAL); // no NOREPLACE/EXCHANGE (2.5)
            }
            b.get(16..).ok_or(EINVAL)?
        } else {
            b.get(8..).ok_or(EINVAL)?
        };
        let (old, rest) = cstr(rest).ok_or(EINVAL)?;
        let (new, _) = cstr(rest).ok_or(EINVAL)?;
        let (pa, _) = self.path_of(h.nodeid)?;
        let (pb, _) = self.path_of(newdir)?;
        let from = scope::join(&pa, old)?;
        let to = scope::join(&pb, new)?;
        match (scope::scope_of(&from), scope::scope_of(&to)) {
            (_, Scope::Refused) => Err(EACCES),
            (Scope::Refused, _) => Err(ENOENT),
            (Scope::Local, Scope::Local) => {
                *effect = true;
                match self.local.rename(&from, &to) {
                    Ok(()) => self.mark_done(h),
                    Err(e)
                        if e == ENOENT
                            && ctl.replay
                            && self.done_before(h)
                            && !self.local.exists(&from)
                            && self.local.exists(&to) =>
                    {
                        replayed_as_done(h, &from);
                    }
                    Err(e) => return Err(e),
                }
                self.answered(h);
                let mut st = lock(&self.st);
                // (a replayed rename whose node already moved moves nothing: renaming again would drop it)
                if !ctl.replay || st.nodes.id_of(&from).is_some() {
                    st.nodes.rename(&from, &to);
                }
                Ok(Vec::new())
            }
            (Scope::Magpie, Scope::Magpie) => {
                self.leased()?;
                let (fe, te) = if ctl.replay {
                    let st = lock(&self.st);
                    let fe = st.tree.get(&from).or_else(|| st.tree.get(&to)).copied().ok_or(ENOENT)?;
                    (fe, None)
                } else {
                    let st = lock(&self.st);
                    let fe = *st.tree.get(&from).ok_or(ENOENT)?;
                    if !st.tree.is_dir(&pb) {
                        return Err(if st.tree.get(&pb).is_some() { ENOTDIR } else { ENOENT });
                    }
                    if from == to {
                        return Ok(Vec::new());
                    }
                    if fe.is_dir() && to.starts_with(&format!("{from}/")) {
                        return Err(EINVAL);
                    }
                    let te = st.tree.get(&to).copied();
                    if let Some(t) = te {
                        if t.is_dir() && !fe.is_dir() {
                            return Err(libc::EISDIR);
                        }
                        if !t.is_dir() && fe.is_dir() {
                            return Err(ENOTDIR);
                        }
                        if t.is_dir() && st.tree.has_children(&to) {
                            return Err(ENOTEMPTY);
                        }
                    }
                    (fe, te)
                };
                let req = CcRename {
                    from: from.clone(),
                    to: to.clone(),
                    op_id: self.op_id(h, 0),
                };
                let v = self
                    .mcall(h, ctl, effect, "cc/rename", |c| {
                        let r = req.clone();
                        async move { c.sb().cc_rename(&r).await }
                    })
                    .map_err(|e| e.errno())?;
                self.answered(h);
                let now = now_ns();
                let mut st = lock(&self.st);
                let moved = st.nodes.id_of(&from);
                // (a replayed rename that had already moved its node moves nothing)
                let dropped = if moved.is_some() {
                    st.nodes.rename(&from, &to)
                } else {
                    Vec::new()
                };
                st.tree.rename(&from, &to, v.version, now);
                let mut content = lock(&self.content);
                for d in dropped {
                    if let Some(t) = te {
                        st.gone.insert(d, t);
                    }
                    content.forget(d);
                }
                if let Some(m) = moved
                    && !fe.is_dir()
                {
                    content.reversion(m, (fe.version, fe.size), v.version);
                }
                if lock(&self.main).as_deref() == Some(to.as_str()) {
                    content.pin(moved);
                }
                Ok(Vec::new())
            }
            _ => Err(EXDEV),
        }
    }

    fn readdir(&self, h: &InHeader, b: &[u8]) -> Result<Vec<u8>, i32> {
        let off = u64_at(b, 8);
        let max = u32_at(b, 16) as usize;
        let (p, _) = self.path_of(h.nodeid)?;
        let mut names: Vec<(String, FileType)> = Vec::new();
        match scope::scope_of(&p) {
            Scope::Top => {
                names.extend(
                    aviary_core::harness::Harness::ALL
                        .iter()
                        .map(|h| (scope::home(*h).to_owned(), FileType::Dir)),
                );
            }
            Scope::Root => {
                let st = lock(&self.st);
                if self.is_leased() {
                    for (n, e) in st.tree.children(&p) {
                        if scope::scope_of(&format!("{p}/{n}")) == Scope::Magpie {
                            names.push((n, if e.is_dir() { FileType::Dir } else { FileType::File }));
                        }
                    }
                }
                drop(st);
                for (n, t) in self.local.readdir(&p)? {
                    if scope::scope_of(&format!("{p}/{n}")) == Scope::Local {
                        names.push((n, t));
                    }
                }
                names.sort();
            }
            Scope::Local => names = self.local.readdir(&p)?,
            Scope::Refused => return Err(ENOENT),
            Scope::Magpie => {
                self.leased()?;
                let st = lock(&self.st);
                if !st.tree.is_dir(&p) {
                    return Err(ENOTDIR);
                }
                for (n, e) in st.tree.children(&p) {
                    names.push((n, if e.is_dir() { FileType::Dir } else { FileType::File }));
                }
            }
        }
        let mut all = vec![
            (".".to_owned(), FileType::Dir, h.nodeid),
            ("..".to_owned(), FileType::Dir, ROOT),
        ];
        {
            let mut st = lock(&self.st);
            for (n, t) in names {
                let child = if p.is_empty() { n.clone() } else { format!("{p}/{n}") };
                let id = st.nodes.assign(&child);
                all.push((n, t, id));
            }
        }
        let mut o = Vec::new();
        #[allow(clippy::cast_possible_truncation)] // directory offsets are small
        for (i, (n, t, id)) in all.iter().enumerate().skip(off as usize) {
            if !proto::push_dirent(&mut o, max, *id, i as u64 + 1, *t, n.as_bytes()) {
                break;
            }
        }
        Ok(o)
    }
}
