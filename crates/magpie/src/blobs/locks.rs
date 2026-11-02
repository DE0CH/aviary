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

use std::sync::Arc;
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
}

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

    async fn session(&self) -> Result<Arc<Session>> {
        let mut cur = self.current.lock().await;
        if let Some(s) = cur.as_ref() {
            return Ok(s.clone());
        }
        let conn = self.pool.get().await?;
        let pid: i32 = conn.query_one("select pg_backend_pid()", &[]).await?.get(0);
        let s = Arc::new(Session { conn, pid });
        *cur = Some(s.clone());
        Ok(s)
    }

    /// Forgets `s` after a statement on it failed: the next lock opens a new session. Locks still held on `s` are
    /// given back when their holders release them or when `s` goes back to its pool (which resets its session).
    async fn broken(&self, s: &Arc<Session>) {
        let mut cur = self.current.lock().await;
        if cur.as_ref().is_some_and(|c| Arc::ptr_eq(c, s)) {
            *cur = None;
        }
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
                self.broken(&s).await;
                Err(e.into())
            }
        }
    }

    /// Takes `sha`'s shared lock (a writer of its object), waiting at most [`SHARED_WAIT`].
    ///
    /// # Errors
    /// 503 `unavailable` when it is not had in time or the session cannot be reached.
    pub async fn shared(&self, sha: &Sha) -> Result<SharedLock> {
        let start = Instant::now();
        loop {
            match self.try_lock(sha, false).await {
                Ok(Some(session)) => return Ok(SharedLock { session, sha: *sha }),
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
    pub async fn try_exclusive(&self, sha: &Sha) -> Result<Option<ExclusiveLock>> {
        Ok(self
            .try_lock(sha, true)
            .await?
            .map(|session| ExclusiveLock { session, sha: *sha }))
    }
}

/// A held shared blob lock ([`LockSession::shared`]); [`SharedLock::release`] gives it back.
#[derive(Debug)]
pub struct SharedLock {
    session: Arc<Session>,
    sha: Sha,
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
                &[&self.session.pid, &LOCK_CLASS, &lock_obj(&self.sha)],
            )
            .await?
            .get(0);
        if held {
            Ok(())
        } else {
            Err(unavailable("the lock session ended while the object was put"))
        }
    }

    /// Releases the lock.
    pub async fn release(self) {
        let _ = self
            .session
            .conn
            .execute(
                "select pg_advisory_unlock_shared($1::int4, $2::int4)",
                &[&LOCK_CLASS, &lock_obj(&self.sha)],
            )
            .await;
    }
}

/// A held exclusive blob lock ([`LockSession::try_exclusive`]).
#[derive(Debug)]
pub struct ExclusiveLock {
    session: Arc<Session>,
    sha: Sha,
}

impl ExclusiveLock {
    /// Whether the lock is still held (its session has not ended): the GC asks before each object it deletes.
    pub async fn held(&self) -> bool {
        self.session
            .conn
            .query_one(
                "select exists (select 1 from pg_locks where locktype = 'advisory' and pid = pg_backend_pid()
                                 and classid = ($1::int4)::oid and objid = ($2::int4)::oid and objsubid = 2
                                 and mode = 'ExclusiveLock' and granted)",
                &[&LOCK_CLASS, &lock_obj(&self.sha)],
            )
            .await
            .is_ok_and(|r| r.get::<_, bool>(0))
    }

    /// Releases the lock.
    pub async fn release(self) {
        let _ = self
            .session
            .conn
            .execute(
                "select pg_advisory_unlock($1::int4, $2::int4)",
                &[&LOCK_CLASS, &lock_obj(&self.sha)],
            )
            .await;
    }
}
