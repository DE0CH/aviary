//! Waiting out a moment without the database (platform 4.3; Deyao, 2026-11-09: a planned move of the primary has zero
//! downtime).
//!
//! A `CloudNativePG` switchover ends every connection to the old primary and refuses new ones until the new primary
//! serves, a few seconds. Instead of failing, the pool's [`crate::Pool::get`] waits for a connection, a statement of a
//! [`crate::Conn`] runs again on a new connection, and a [`crate::tx!`] whose connection was lost before its `COMMIT`
//! was sent runs again from the start, every [`RETRY_GAP`] for up to [`RETRY_FOR`]. Running again is safe: the
//! switchover fences writes first (YYC `news-stream/deploy/clusters/talos/db/switchover.sh` locks every table
//! `EXCLUSIVE`, so no write transaction is open when the old primary stops), so a cut statement or transaction was
//! reading or waiting before its first write. A transaction whose `COMMIT` was in flight is never run again: its
//! outcome is unknown, and the error goes to the caller.

use std::time::{Duration, Instant};

use deadpool_postgres::Object;

use crate::Error;

/// How long a piece of work keeps trying while the database is away.
pub const RETRY_FOR: Duration = Duration::from_secs(30);
/// The pause between tries.
pub const RETRY_GAP: Duration = Duration::from_millis(200);

/// The deadline of one piece of work's tries: set at its first transient failure.
#[derive(Debug, Default)]
pub(crate) struct Patience(Option<Instant>);

impl Patience {
    /// After a transient failure `e` of `what`: `true` when it may try again (after [`RETRY_GAP`]).
    pub(crate) fn again(&mut self, what: &str, e: &Error) -> bool {
        let now = Instant::now();
        match self.0 {
            None => {
                self.0 = Some(now + RETRY_FOR);
                tracing::warn!(error = %e, what, "database away; trying again for up to 30 s");
                true
            }
            Some(end) => now < end,
        }
    }
}

/// A pooled connection, waiting while the database is away (the pool's own wait and connect timeouts are 5 s).
pub(crate) async fn get(pool: &deadpool_postgres::Pool) -> Result<Object, Error> {
    let mut patience = Patience::default();
    loop {
        match pool.get().await {
            Ok(o) => return Ok(o),
            Err(e) => {
                let e = Error::from(e);
                if !e.is_transient() || !patience.again("connection", &e) {
                    return Err(e);
                }
                tokio::time::sleep(RETRY_GAP).await;
            }
        }
    }
}
