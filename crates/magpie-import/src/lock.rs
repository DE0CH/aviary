//! The importer's leader lock (cutover 7.1): `pg_advisory_lock(hashtext('magpie-import.leader'))` on the magpie
//! database. The hand-over Deployment's replicas bid for it with [`leader`] and hold it for the process's life (the
//! one without it is ready and waits); a one-shot command (`magpie import <cmd>` by `kubectl exec` or a Job) takes
//! it with [`OneShot::take`], which waits in `pg_locks` as an ungranted `pg_advisory_lock`, so the loop sees a
//! waiter ([`waiter_present`]) and lets go between two passes. Only one writer of the importer's state runs at a time.

use std::time::Duration;

use aviary_pg::{Conn, LockKey, Pool};

use crate::Result;

/// The lock's name.
pub const NAME: &str = "magpie-import.leader";

/// How often a follower bids (platform 4.3 is 5 s; the importer's pass is 5 s, so a successor takes over within
/// one pass).
pub const RETRY: Duration = Duration::from_secs(1);

/// The key.
#[must_use]
pub fn key() -> LockKey {
    LockKey::name(NAME)
}

/// Bids for the lock on a dedicated connection (the hand-over Deployment).
#[must_use]
pub fn leader(pool: &Pool) -> aviary_pg::Leader {
    aviary_pg::Leader::start_with(pool, key(), RETRY)
}

/// Whether a one-shot command waits for the lock now (an ungranted advisory lock on the key in `pg_locks`).
///
/// # Errors
/// A statement error.
pub async fn waiter_present(pool: &Pool) -> Result<bool> {
    Ok(pool
        .get()
        .await?
        .query_one(
            "select exists (select 1 from pg_locks l
                             where l.locktype = 'advisory' and not l.granted and l.objsubid = 1
                               and l.database = (select oid from pg_database where datname = current_database())
                               and ((l.classid::bigint << 32) | l.objid::bigint) = hashtext($1)::bigint)",
            &[&NAME],
        )
        .await?
        .get(0))
}

/// The lock held by a one-shot command, on one pooled connection; [`OneShot::release`] gives it back.
#[derive(Debug)]
pub struct OneShot {
    conn: Option<Conn>,
    /// The backend that holds it.
    pid: i32,
}

/// Whether backend `pid` holds the lock, from inside another connection's transaction.
const HELD_BY: &str = "select exists (select 1 from pg_locks l
                        where l.locktype = 'advisory' and l.granted and l.pid = $2 and l.objsubid = 1
                          and ((l.classid::bigint << 32) | l.objid::bigint) = hashtext($1)::bigint)";

impl OneShot {
    /// Waits for the lock (`pg_advisory_lock`, retried when the statement timeout ends a long wait).
    ///
    /// # Errors
    /// A connection error.
    pub async fn take(pool: &Pool) -> Result<Self> {
        loop {
            let conn = pool.get().await?;
            match conn
                .query_one("select pg_advisory_lock(hashtext($1)), pg_backend_pid()", &[&NAME])
                .await
            {
                Ok(r) => {
                    return Ok(Self {
                        conn: Some(conn),
                        pid: r.get(1),
                    });
                }
                Err(e) if e.code() == Some(&aviary_pg::reexports::tokio_postgres::error::SqlState::QUERY_CANCELED) => {
                    tracing::info!("still waiting for the importer's lock");
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Whether the lock is still held: its connection answers and `pg_locks` shows this backend granted it. A
    /// connection that broke (a server restart, the network, an idle kill) took the lock with it, and the hand-over
    /// leader may hold it now: the command must stop writing ([`crate::Importer::check_lock`]).
    pub async fn held(&self) -> bool {
        let Some(c) = &self.conn else { return false };
        match c
            .query_one(
                "select exists (select 1 from pg_locks l
                                 where l.locktype = 'advisory' and l.granted and l.pid = pg_backend_pid()
                                   and l.objsubid = 1
                                   and ((l.classid::bigint << 32) | l.objid::bigint) = hashtext($1)::bigint)",
                &[&NAME],
            )
            .await
        {
            Ok(r) => r.get(0),
            Err(e) => {
                tracing::error!(error = %e, "the importer's lock connection failed");
                false
            }
        }
    }

    /// Whether the lock is still held, checked inside a write transaction `t` on another connection: the write
    /// commits only if the lock's backend still held it at that statement, which narrows a loss between
    /// [`OneShot::held`] and the write to that transaction's own commit. Fails with [`crate::Error::LockLost`].
    ///
    /// # Errors
    /// [`crate::Error::LockLost`]; a statement error.
    pub async fn check_in(&self, t: &aviary_pg::Tx<'_>) -> Result<()> {
        if self.conn.is_none() || !t.query_one(HELD_BY, &[&NAME, &self.pid]).await?.get::<_, bool>(0) {
            return Err(crate::Error::LockLost);
        }
        Ok(())
    }

    /// Releases the lock. If the unlock fails on a connection that still answers, the pool's recycle (its
    /// `pg_advisory_unlock_all()`, when the connection is next taken) or the end of the connection lets go of it.
    pub async fn release(mut self) {
        if let Some(c) = self.conn.take()
            && let Err(e) = c.query_one("select pg_advisory_unlock(hashtext($1))", &[&NAME]).await
        {
            tracing::warn!(error = %e, "the importer's lock was not released (it goes with the connection)");
            let _ = c.query_one("select pg_advisory_unlock_all()", &[]).await;
        }
    }
}
