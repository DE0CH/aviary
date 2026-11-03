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
}

impl OneShot {
    /// Waits for the lock (`pg_advisory_lock`, retried when the statement timeout ends a long wait).
    ///
    /// # Errors
    /// A connection error.
    pub async fn take(pool: &Pool) -> Result<Self> {
        loop {
            let conn = pool.get().await?;
            match conn.query_one("select pg_advisory_lock(hashtext($1))", &[&NAME]).await {
                Ok(_) => return Ok(Self { conn: Some(conn) }),
                Err(e) if e.code() == Some(&aviary_pg::reexports::tokio_postgres::error::SqlState::QUERY_CANCELED) => {
                    tracing::info!("still waiting for the importer's lock");
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Releases the lock.
    pub async fn release(mut self) {
        if let Some(c) = self.conn.take()
            && let Err(e) = c.query_one("select pg_advisory_unlock(hashtext($1))", &[&NAME]).await
        {
            tracing::warn!(error = %e, "the importer's lock was not released (it goes with the connection)");
        }
    }
}
