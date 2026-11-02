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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
use super::scope::{self, Scope, Unclassified};
use super::{Config, Device, Hooks, Point, StuckOp, now_ns};

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
    /// pid → (kept slot, record, order).
    kept: HashMap<u32, (usize, KeptRecord, u64)>,
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
    resend: Mutex<Resend>,
    ctls: Mutex<HashMap<u64, Arc<OpCtl>>>,
    inflight: AtomicU32,
    idle: (Mutex<()>, Condvar),
    hooks: RwLock<Hooks>,
    relocking: AtomicBool,
    /// A write answered EDQUOT (the heartbeat's `cc.quotaExceeded`, 9.10).
    quota_exceeded: AtomicBool,
    dead: AtomicBool,
    stopped: AtomicBool,
    started_ns: i64,
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Core").field("boot", &self.boot).finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
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
            resend: Mutex::new(Resend::default()),
            ctls: Mutex::new(HashMap::new()),
            inflight: AtomicU32::new(0),
            idle: (Mutex::new(()), Condvar::new()),
            hooks: RwLock::new(a.hooks),
            relocking: AtomicBool::new(false),
            quota_exceeded: AtomicBool::new(false),
            dead: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            started_ns: now_ns(),
            cfg,
        });
        core.remote.set_lease(Some(Lease {
            id: a.lease.lease_id,
            epoch: a.lease.epoch,
        }));
        core.fetch_tree();
        if a.restart {
            core.restore_kept();
            core.replay();
        }
        core.retain_nodes();
        core.spawn_workers();
        core.spawn_saver();
        if !a.restart {
            let c = core.clone();
            std::thread::spawn(move || c.prefetch_main());
        }
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
                c2.remote.set_lease(None);
                c2.load_tree();
                lock(&c2.main).clone_from(&l2.main_transcript);
                c2.remote.set_lease(Some(Lease {
                    id: l2.lease_id,
                    epoch: l2.epoch,
                }));
            })
            .await;
            c.relocking.store(false, Ordering::SeqCst);
            tracing::info!(epoch = lease.epoch, "lease taken again");
            (c.hooks().lease_regained)(lease);
        });
    }

    /// After a restore (or a clock jump): re-load the tree and revalidate the cache.
    pub fn resumed(&self) {
        self.load_tree();
    }

    // ---- the tree, the cache, the repair ----

    fn load_tree(&self) {
        self.fetch_tree();
        self.retain_nodes();
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
            Scope::Root => true,
            Scope::Refused => false,
        });
    }

    fn fetch_tree(&self) {
        let ctl = OpCtl::default();
        let t = self.rt.block_on(self.remote.read(
            &ctl,
            "cc/tree",
            |c| async move { c.client.sandbox(None).cc_tree().await },
            &|| {},
        ));
        let Ok(t) = t else {
            return;
        };
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
    }

    /// 9.5 item 6 / magpie 6.12: a cache file newer and longer than magpie's copy whose first bytes equal magpie's
    /// is re-sent from magpie's end with fresh `opId`s. (A repeated repair after a crash is harmless: magpie answers
    /// a write whose range already holds those bytes with success, rule 0.)
    fn repair_candidate(&self, c: &Candidate) -> bool {
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
            Some(v) => {
                lock(&self.content).adopt(c, v);
                true
            }
            None => {
                lock(&self.content).discard(c);
                false
            }
        }
    }

    /// Compares magpie's last 64 KiB with the cached copy and re-sends `[magpie.size, local)`; magpie's new version.
    fn resend_suffix(&self, path: &str, file: &std::path::Path, e: Entry, local: u64) -> Option<u64> {
        if e.size >= local {
            return None;
        }
        let from = e.size.saturating_sub(64 * 1024);
        let ctl = OpCtl::default();
        let theirs = self.fetch_range(&ctl, path, from, e.size - from).ok()?;
        #[allow(clippy::cast_possible_truncation)] // 64 KiB
        let ours = cache::read_at(file, from, (e.size - from) as usize).ok()?;
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
            return None;
        }
        let mut off = e.size;
        let mut version = e.version;
        while off < local {
            #[allow(clippy::cast_possible_truncation)] // a chunk is at most 1 MiB
            let n = ((local - off) as usize).min(self.cfg.max_write as usize);
            let data = Bytes::from(cache::read_at(file, off, n).ok()?);
            let q = CcWriteQuery {
                path: path.to_owned(),
                offset: off,
                mtime_ns: now_ns(),
                op_id: Uuid::new_v4(),
                create: false,
                mode: e.mode,
                turn_id: None,
                append: false,
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
            match r {
                Ok(sv) => {
                    version = sv.version;
                    off += n as u64;
                }
                Err(err) => {
                    tracing::error!(path, offset = off, error = ?err, "repair write failed");
                    return None;
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
        Some(version)
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

    /// The whole file into the cache (streamed to disk); the cache file's path.
    fn fetch_whole(&self, ctl: &OpCtl, node: u64, path: &str, e: Entry) -> Result<std::path::PathBuf, i32> {
        let staging = lock(&self.content).staging(node);
        let q = CcFileQuery {
            path: path.to_owned(),
            offset: None,
            length: None,
        };
        let st = staging.clone();
        let r = self.rt.block_on(self.remote.read(
            ctl,
            "cc/file",
            |c| {
                let q = q.clone();
                let st = st.clone();
                async move {
                    let mut r = c.client.sandbox(None).cc_file(&q).await?;
                    let mut f = std::fs::File::create(&st).map_err(|e| magpie_api::client::ClientError::Decode {
                        route: "cc_file",
                        error: format!("cache file: {e}"),
                    })?;
                    let mut n = 0u64;
                    while let Some(chunk) = r.chunk().await.map_err(magpie_api::client::ClientError::Http)? {
                        use std::io::Write;
                        f.write_all(&chunk)
                            .map_err(|e| magpie_api::client::ClientError::Decode {
                                route: "cc_file",
                                error: format!("cache file: {e}"),
                            })?;
                        n += chunk.len() as u64;
                    }
                    Ok(n)
                }
            },
            &|| {},
        ));
        match r {
            Ok(n) if n == e.size => {
                let mut c = lock(&self.content);
                c.filled(node, e.version, e.size, &staging);
                c.valid(node, e.version, e.size).ok_or(EIO)
            }
            Ok(n) => {
                let _ = std::fs::remove_file(&staging);
                tracing::warn!(
                    path,
                    got = n,
                    want = e.size,
                    "a fetched file's size differs from the tree"
                );
                Err(EIO)
            }
            Err(err) => {
                let _ = std::fs::remove_file(&staging);
                Err(err.errno())
            }
        }
    }

    fn prefetch_main(&self) {
        let Some(m) = lock(&self.main).clone() else { return };
        let (node, e) = {
            let mut st = lock(&self.st);
            let Some(e) = st.tree.get(&m).copied() else { return };
            (st.nodes.assign(&m), e)
        };
        lock(&self.content).pin(Some(node));
        if lock(&self.content).valid(node, e.version, e.size).is_none() {
            let _ = self.fetch_whole(&OpCtl::default(), node, &m, e);
        }
    }

    fn spawn_saver(self: &Arc<Self>) {
        let c = Arc::downgrade(self);
        let path = self.cfg.state.join("tree.json");
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(5));
                let Some(c) = c.upgrade() else { return };
                if c.stopped.load(Ordering::SeqCst) || c.died() {
                    return;
                }
                lock(&c.st).tree.save_if_dirty(&path);
            }
        });
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
            tracing::warn!(point = ?p, opcode = proto::opname(h.opcode), "fault-injection: SIGKILL");
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
                    _ => {
                        if !self.stopped.load(Ordering::SeqCst) && !self.died() {
                            tracing::info!(slot = i, error = %e, "fuse read ended");
                        }
                        return;
                    }
                },
            };
            if !self.journal.whole_request(i, slot, n) {
                continue;
            }
            let Some(h) = InHeader::parse(slot) else { continue };
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
            if let Some((k, rec, _)) = rs.kept.remove(&h.pid) {
                self.journal.unkeep(k);
                if (rec.opcode, rec.nodeid, rec.key) == key {
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
                    .is_some_and(|(_, r, _)| (r.opcode, r.nodeid, r.key) == key)
                    && let Some((k, _, _)) = rs.kept.remove(&h.pid)
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
        let werr = self.send(h, &o);
        let mut rs = lock(&self.resend);
        if let Some(list) = rs.inflight.get_mut(&h.pid) {
            list.retain(|i| !Arc::ptr_eq(i, &me));
            if list.is_empty() {
                rs.inflight.remove(&h.pid);
            }
        }
        if werr == ENOENT && o.effect && !me.matched.load(Ordering::SeqCst) {
            self.keep_locked(&mut rs, h, key, &o);
        }
    }

    fn keep_locked(&self, rs: &mut Resend, h: &InHeader, key: Key, o: &Outcome) {
        if let Some((k, _, _)) = rs.kept.remove(&h.pid) {
            self.journal.unkeep(k);
        }
        if rs.kept.len() >= KEPT_SLOTS
            && let Some(oldest) = rs.kept.iter().min_by_key(|(_, v)| v.2).map(|(p, _)| *p)
            && let Some((k, _, _)) = rs.kept.remove(&oldest)
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
        rs.kept.insert(h.pid, (k, rec, ord));
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
            if let Some((old, _, _)) = rs.kept.insert(rec.pid, (k, rec, ord)) {
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
            let o = self.serve_as(&h, &body, true);
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

    fn leased(&self) -> Result<(), i32> {
        if self.remote.lease().is_some() {
            Ok(())
        } else {
            Err(EIO)
        }
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
            Scope::Root => Ok(self.root_attr()),
            Scope::Refused => Err(ENOENT),
            Scope::Local => self.local.getattr(ino, path, handle),
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
                        self.local.symlink(target, &p)?;
                        let mut a = self.local.getattr(0, &p, None)?;
                        a.ino = lock(&self.st).nodes.assign(&p);
                        Ok(self.entry(&a))
                    }
                    Scope::Refused => Err(EACCES),
                    _ => Err(EPERM),
                }
            }
            op::MKNOD | op::LINK => Err(EPERM),
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
            op::SETXATTR | op::REMOVEXATTR => Err(ENOTSUP),
            op::FALLOCATE => Err(EOPNOTSUPP),
            _ => Err(ENOSYS),
        }
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
        match &r {
            Ok(_) => self.quota_exceeded.store(false, Ordering::SeqCst),
            Err(CallError::Errno(e)) if *e == libc::EDQUOT => {
                tracing::warn!(what, "magpie quota exceeded");
                self.quota_exceeded.store(true, Ordering::SeqCst);
            }
            Err(CallError::LeaseLost) => self.on_lease_lost(),
            Err(CallError::Stuck(msg)) => {
                (self.hooks().write_stuck)(StuckOp {
                    what: what.to_owned(),
                    message: msg.clone(),
                });
            }
            _ => {}
        }
        r
    }

    fn answered(&self, h: &InHeader) {
        self.point(Point::CallAnswered, h);
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
            Scope::Root => return Ok(proto::attr_out(&self.root_attr(), self.cfg.attr_valid)),
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

    fn repair_file(&self, path: &str) -> bool {
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
            Some(v) => {
                lock(&self.content).reversion(node, (o.version, o.size), v);
                true
            }
            None => {
                if e.size >= o.size {
                    (self.hooks().forked)(format!("{path}: magpie has {} bytes, the cache {}", e.size, o.size));
                }
                false
            }
        }
    }

    fn open(self: &Arc<Self>, h: &InHeader, b: &[u8], ctl: &OpCtl, effect: &mut bool) -> Result<Vec<u8>, i32> {
        #[allow(clippy::cast_possible_wrap)] // open(2) flags
        let flags = u32_at(b, 0) as i32;
        let (p, gone) = self.path_of(h.nodeid)?;
        match scope::scope_of(&p) {
            Scope::Root => Ok(proto::open_out(fh::of(h.nodeid, flags))),
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
            Scope::Local | Scope::Root => {
                *effect = true;
                let handle = self.local.create(|| lock(&self.st).nodes.assign(&p), &p, flags, mode)?;
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
            Scope::Root => Err(libc::EISDIR),
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
                let cached = lock(&self.content).valid(node, e.version, e.size);
                let file = match cached {
                    Some(f) => Ok(f),
                    None if e.size <= magpie_api::sandbox::CC_FILE_MAX_BYTES => {
                        self.fetch_whole(&OpCtl::default(), node, &p, e)
                    }
                    None => Err(EFBIG),
                };
                match file {
                    Ok(f) => cache::read_at(&f, off, size as usize).map_err(|_| EIO),
                    Err(err) if err == EIO => {
                        // the cache cannot hold it (disk full): serve the range from magpie
                        let n = u64::from(size).min(e.size - off);
                        self.fetch_range(&OpCtl::default(), &p, off, n)
                    }
                    Err(err) => Err(err),
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
            Scope::Root => Err(libc::EISDIR),
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
                    append: false,
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
            Scope::Local | Scope::Root => {
                *effect = true;
                self.local.mkdir(&p, mode)?;
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
            Scope::Local | Scope::Root => {
                *effect = true;
                if rmdir {
                    self.local.rmdir(&p)?;
                } else {
                    self.local.unlink(&p)?;
                }
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
                self.local.rename(&from, &to)?;
                lock(&self.st).nodes.rename(&from, &to);
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
            Scope::Root => {
                let st = lock(&self.st);
                if self.remote.lease().is_some() {
                    for (n, e) in st.tree.children("") {
                        if scope::scope_of(&n) == Scope::Magpie {
                            names.push((n, if e.is_dir() { FileType::Dir } else { FileType::File }));
                        }
                    }
                }
                drop(st);
                for (n, t) in self.local.readdir("")? {
                    if scope::scope_of(&n) == Scope::Local {
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
