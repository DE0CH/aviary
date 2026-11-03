//! The blobs' advisory locks (see [`super`]), held on lock sessions of their own rather than on connections of the
//! request pool.
//!
//! A writer holds its blob's shared lock for as long as its bucket `PUT` takes, and a bucket that hangs instead of
//! refusing keeps a `PUT` going for minutes (60 s per attempt). A lock held on a pooled request connection would pin
//! that connection for as long, and a few hanging uploads would take the whole request pool from the cc writes and
//! every other request. So every lock of a replica's writers sits on one session (a connection of a small pool of its
//! own, [`LOCK_POOL_SIZE`]): any number of shared locks stack on it, and taking or giving one back is one short
//! statement. The GC's exclusive locks sit on a second session, since one session's locks never conflict with each
//! other.
//!
//! Locks are taken with `pg_try_advisory_lock*` and retried, never by a statement that waits, so one wait cannot
//! hold up the session for everyone else. A session that breaks loses its locks; a writer therefore checks, in the
//! transaction that records its blob ([`SharedLock::check`]), that its lock is still held (Postgres' `pg_locks`),
//! and answers 503 when it is not.
//!
//! A held lock is given back when its holder drops it, not only by [`SharedLock::release`]: a request whose client
//! goes away mid-`PUT` drops its future, and a lock left on the long-lived session would stay held until the replica
//! restarts (the GC could then never free that blob, nor any other whose sha shares its first four bytes). Taking a
//! lock runs in a task of its own, so a caller dropped while the statement is in flight still gets the lock given
//! back; dropping a [`SharedLock`] or [`ExclusiveLock`] spawns its unlock on the session.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aviary_pg::{Conn, Pool};
use tokio::sync::Mutex;

use super::{LOCK_CLASS, Sha, lock_obj};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::limits;

/// Connections of one lock session's pool: the session, and a replacement while a broken one is still held.
pub const LOCK_POOL_SIZE: usize = 2;

/// How long a writer waits for its shared lock (only the GC's exclusive lock, held while it deletes a blob's
/// objects, keeps it waiting) before answering 503.
pub const SHARED_WAIT: Duration = Duration::from_secs(30);

const RETRY: Duration = Duration::from_millis(20);

#[derive(Debug)]
struct Session {
    conn: Conn,
    pid: i32,
    /// A statement on it failed or a lock on it was found lost: it has ended, and the next lock opens a new one.
    broken: AtomicBool,
}

impl Session {
    fn mark_broken(&self) {
        self.broken.store(true, Ordering::Relaxed);
    }
}

/// How long opening a lock session waits for a connection of its pool (both may still be held by a broken session's
/// holders) before the lock answers 503.
const SESSION_WAIT: Duration = Duration::from_secs(5);

/// One lock session of a replica (opened on first use, replaced when it breaks).
pub struct LockSession {
    pool: Pool,
    current: Mutex<Option<Arc<Session>>>,
}

impl std::fmt::Debug for LockSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockSession").finish_non_exhaustive()
    }
}

fn unavailable(why: &str) -> Error {
    tracing::warn!(why, "blob lock not taken");
    Error::code(
        magpie_api::errors::ErrorCode::Unavailable,
        "the blob lock could not be taken",
    )
}

impl LockSession {
    /// A lock session on the database at `url` (no connection is opened yet).
    ///
    /// # Errors
    /// When the URL does not parse.
    pub fn new(url: &str, app_name: &str) -> Result<Self> {
        Ok(Self {
            pool: aviary_pg::connect(url, app_name, LOCK_POOL_SIZE, limits::STATEMENT_TIMEOUT)?,
            current: Mutex::new(None),
        })
    }

    /// The current session, or a new one when there is none or it is broken. The connection is taken without the
    /// `current` mutex held (and waits at most [`SESSION_WAIT`]), so a pool whose connections are both still held
    /// by a broken session's holders keeps only this lock waiting, not every other lock of the session.
    async fn session(&self) -> Result<Arc<Session>> {
        if let Some(s) = self.current.lock().await.as_ref()
            && !s.broken.load(Ordering::Relaxed)
        {
            return Ok(s.clone());
        }
        let conn = tokio::time::timeout(SESSION_WAIT, self.pool.get())
            .await
            .map_err(|_| unavailable("no connection for a new lock session"))??;
        let pid: i32 = conn.query_one("select pg_backend_pid()", &[]).await?.get(0);
        let mut cur = self.current.lock().await;
        if let Some(s) = cur.as_ref()
            && !s.broken.load(Ordering::Relaxed)
        {
            // another lock opened one meanwhile; this connection goes back to the pool
            return Ok(s.clone());
        }
        let s = Arc::new(Session {
            conn,
            pid,
            broken: AtomicBool::new(false),
        });
        *cur = Some(s.clone());
        Ok(s)
    }

    async fn try_lock(&self, sha: &Sha, exclusive: bool) -> Result<Option<Arc<Session>>> {
        let s = self.session().await?;
        let sql = if exclusive {
            "select pg_try_advisory_lock($1::int4, $2::int4)"
        } else {
            "select pg_try_advisory_lock_shared($1::int4, $2::int4)"
        };
        match s.conn.query_one(sql, &[&LOCK_CLASS, &lock_obj(sha)]).await {
            Ok(r) => Ok(r.get::<_, bool>(0).then_some(s)),
            Err(e) => {
                // locks still held on `s` are given back when their holders release them or when `s` goes back to
                // its pool (which resets its session)
                s.mark_broken();
                Err(e.into())
            }
        }
    }

    /// Takes `sha`'s shared lock (a writer of its object), waiting at most [`SHARED_WAIT`].
    ///
    /// # Errors
    /// 503 `unavailable` when it is not had in time or the session cannot be reached.
    pub async fn shared(self: &Arc<Self>, sha: &Sha) -> Result<SharedLock> {
        let me = self.clone();
        let sha = *sha;
        // in a task of its own: a caller dropped meanwhile drops the lock the task took, which gives it back
        tokio::spawn(async move { me.shared_now(&sha).await })
            .await
            .map_err(|e| Error::Internal(format!("blob lock task: {e}")))?
    }

    async fn shared_now(&self, sha: &Sha) -> Result<SharedLock> {
        let start = Instant::now();
        loop {
            match self.try_lock(sha, false).await {
                Ok(Some(session)) => {
                    return Ok(SharedLock {
                        held: Some(session),
                        sha: *sha,
                    });
                }
                Ok(None) if start.elapsed() < SHARED_WAIT => tokio::time::sleep(RETRY).await,
                Ok(None) => return Err(unavailable("the GC held it")),
                Err(e) => {
                    tracing::warn!(error = %e, "blob lock session failed");
                    return Err(unavailable("the lock session failed"));
                }
            }
        }
    }

    /// Takes `sha`'s exclusive lock (the GC) if no writer holds it; `None` when one does.
    ///
    /// # Errors
    /// When the session cannot be reached.
    pub async fn try_exclusive(self: &Arc<Self>, sha: &Sha) -> Result<Option<ExclusiveLock>> {
        let me = self.clone();
        let sha = *sha;
        tokio::spawn(async move {
            Ok(me.try_lock(&sha, true).await?.map(|session| ExclusiveLock {
                held: Some(session),
                sha,
            }))
        })
        .await
        .map_err(|e| Error::Internal(format!("blob lock task: {e}")))?
    }
}

const UNLOCK_SHARED: &str = "select pg_advisory_unlock_shared($1::int4, $2::int4)";
const UNLOCK_EXCLUSIVE: &str = "select pg_advisory_unlock($1::int4, $2::int4)";

/// Gives back one hold of `sha`'s lock on `session`, in a task of its own (so neither a dropped caller nor a drop
/// outside an async context can skip it). A broken session has lost its locks already; the error is ignored.
fn unlock(session: Arc<Session>, sha: Sha, sql: &'static str) -> Option<tokio::task::JoinHandle<()>> {
    let rt = tokio::runtime::Handle::try_current().ok()?;
    Some(rt.spawn(async move {
        let _ = session.conn.execute(sql, &[&LOCK_CLASS, &lock_obj(&sha)]).await;
    }))
}

/// A held shared blob lock ([`LockSession::shared`]); [`SharedLock::release`] or a drop gives it back.
#[derive(Debug)]
pub struct SharedLock {
    held: Option<Arc<Session>>,
    sha: Sha,
}

impl Drop for SharedLock {
    fn drop(&mut self) {
        if let Some(s) = self.held.take() {
            unlock(s, self.sha, UNLOCK_SHARED);
        }
    }
}

impl Drop for ExclusiveLock {
    fn drop(&mut self) {
        if let Some(s) = self.held.take() {
            unlock(s, self.sha, UNLOCK_EXCLUSIVE);
        }
    }
}

impl SharedLock {
    /// Checks, inside the transaction that records the blob, that the lock is still held (its session has not
    /// ended). Run it after the statements that take the blob row's lock, so a GC that comes after the session ended
    /// waits for this transaction and then finds the blob just referenced.
    ///
    /// # Errors
    /// 503 `unavailable` when the lock was lost; a statement error.
    pub async fn check(&self, db: &impl Db) -> Result<()> {
        let held: bool = db
            .query_one(
                "select exists (select 1 from pg_locks where locktype = 'advisory' and pid = $1
                                 and classid = ($2::int4)::oid and objid = ($3::int4)::oid and objsubid = 2
                                 and mode = 'ShareLock' and granted)",
                &[&self.pid(), &LOCK_CLASS, &lock_obj(&self.sha)],
            )
            .await?
            .get(0);
        if held {
            Ok(())
        } else {
            if let Some(s) = self.held.as_ref() {
                s.mark_broken();
            }
            Err(unavailable("the lock session ended while the object was put"))
        }
    }

    fn pid(&self) -> i32 {
        self.held.as_ref().map_or(0, |s| s.pid)
    }

    /// Releases the lock (and waits until it is given back).
    pub async fn release(mut self) {
        if let Some(s) = self.held.take()
            && let Some(h) = unlock(s, self.sha, UNLOCK_SHARED)
        {
            let _ = h.await;
        }
    }
}

/// A held exclusive blob lock ([`LockSession::try_exclusive`]).
#[derive(Debug)]
pub struct ExclusiveLock {
    held: Option<Arc<Session>>,
    sha: Sha,
}

impl ExclusiveLock {
    /// Whether the lock is still held (its session has not ended): the GC asks before each object it deletes.
    pub async fn held(&self) -> bool {
        let Some(s) = self.held.as_ref() else {
            return false;
        };
        let held = s
            .conn
            .query_one(
                "select exists (select 1 from pg_locks where locktype = 'advisory' and pid = pg_backend_pid()
                                 and classid = ($1::int4)::oid and objid = ($2::int4)::oid and objsubid = 2
                                 and mode = 'ExclusiveLock' and granted)",
                &[&LOCK_CLASS, &lock_obj(&self.sha)],
            )
            .await
            .is_ok_and(|r| r.get::<_, bool>(0));
        if !held {
            // the session ended (or its connection failed): the next lock opens a new one
            s.mark_broken();
        }
        held
    }

    /// Releases the lock (and waits until it is given back).
    pub async fn release(mut self) {
        if let Some(s) = self.held.take()
            && let Some(h) = unlock(s, self.sha, UNLOCK_EXCLUSIVE)
        {
            let _ = h.await;
        }
    }
}
