//! Live fan-out across replicas (magpie 14): one dedicated LISTEN connection per replica on four channels, and the
//! long-poll loop every held request uses.
//!
//! Notifications only say "look now": the rows are the truth, so a duplicated or coalesced notification is harmless.
//! A waiter is made before the query whose answer it waits to change, a held poll re-queries every second
//! regardless, and after a LISTEN reconnect every waiter wakes (aviary-pg's [`Listener`]), so nothing committed while
//! the connection was down is missed. A draining replica answers every held poll at once with what it has.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use aviary_core::drain::Draining;
use aviary_pg::{Listener, Pool, Tx, Waiter};

use crate::error::Result;
use crate::limits;

/// Rows written or the epoch bumped: payload `<conversation uuid>:<row_head>`.
pub const ROWS: &str = "magpie_rows";
/// A change-feed insert: payload `<person_key>:<change_seq>`.
pub const CHANGES: &str = "magpie_changes";
/// A command queued or canceled: payload `<conversation uuid>:<cmd_head>`.
pub const COMMANDS: &str = "magpie_commands";
/// A live-frame insert: payload `<conversation uuid>:<n>`.
pub const LIVE: &str = "magpie_live";

/// Every channel magpie listens on.
pub const CHANNELS: [&str; 4] = [ROWS, CHANGES, COMMANDS, LIVE];

/// Sends `<key>:<head>` on `channel`, delivered when `tx` commits (never outside magpie).
///
/// # Errors
/// When the statement fails.
pub async fn notify(tx: &Tx<'_>, channel: &str, key: &str, head: i64) -> Result<()> {
    tx.notify(channel, &format!("{key}:{head}")).await?;
    Ok(())
}

/// The replica's LISTEN connection and its held polls.
#[derive(Debug)]
pub struct Fanout {
    listener: Listener,
    held: Arc<AtomicI64>,
}

impl Fanout {
    /// Starts listening on [`CHANNELS`]; [`Fanout::ready`] resolves once the first connection listens.
    ///
    /// # Errors
    /// Never for the fixed channel names.
    pub fn start(pool: &Pool) -> Result<Self> {
        Ok(Self {
            listener: Listener::start(pool, &CHANNELS)?,
            held: Arc::new(AtomicI64::new(0)),
        })
    }

    /// Resolves once the first LISTEN connection is up (readiness waits for it, platform 3.2).
    pub async fn ready(&self) {
        self.listener.ready().await;
    }

    /// A waiter for `key` on `channel`.
    #[must_use]
    pub fn waiter(&self, channel: &str, key: &str) -> Waiter {
        self.listener.waiter(channel, key)
    }

    /// How many long-polls this replica holds now (a metric).
    #[must_use]
    pub fn held(&self) -> i64 {
        self.held.load(Ordering::Relaxed)
    }

    /// How often the LISTEN connection was re-established (a metric).
    #[must_use]
    pub fn reconnects(&self) -> u64 {
        self.listener.reconnects()
    }

    /// Closes the LISTEN connection (drain).
    pub async fn stop(self) {
        self.listener.stop().await;
    }

    /// The long-poll loop: runs `query` until it answers `Some`, then returns it; when `wait` has passed, or the
    /// replica is draining, returns `query`'s last `None` turned into `empty()`. The waiter on `channel`/`key` is made
    /// before the first query, and the loop re-queries at every notification, every [`limits::REQUERY`] and
    /// after a LISTEN reconnect.
    ///
    /// # Errors
    /// `query`'s error.
    pub async fn hold<T, F, Fut>(
        &self,
        channel: &str,
        key: &str,
        wait: Duration,
        draining: &Draining,
        mut query: F,
        empty: impl FnOnce() -> T,
    ) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<Option<T>>>,
    {
        let wait = wait.min(limits::WAIT_MAX);
        let mut waiter = self.waiter(channel, key);
        let deadline = tokio::time::Instant::now() + wait;
        let mut draining = draining.clone();
        let _held = Held::new(self.held.clone());
        loop {
            if let Some(t) = query().await? {
                return Ok(t);
            }
            let now = tokio::time::Instant::now();
            if now >= deadline || draining.is_draining() {
                return Ok(empty());
            }
            let pause = (deadline - now).min(limits::REQUERY);
            tokio::select! {
                () = waiter.changed() => {}
                () = tokio::time::sleep(pause) => {}
                () = draining.wait() => {}
            }
        }
    }
}

/// Counts a held poll while it lives.
struct Held(Arc<AtomicI64>);

impl Held {
    fn new(n: Arc<AtomicI64>) -> Self {
        n.fetch_add(1, Ordering::Relaxed);
        Self(n)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}
