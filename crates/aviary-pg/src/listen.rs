//! One dedicated LISTEN connection per process, with reconnect and "wake everyone" (platform 4.3, magpie 14, hermit
//! 10.4).
//!
//! Notifications only say "look now": the rows are the truth. A waiter is made *before* the query whose answer it
//! waits to change, so a notification between the query and the wait is not lost:
//!
//! ```ignore
//! let mut w = listener.waiter("magpie_rows", &conversation.to_string());
//! loop {
//!     let rows = read_rows(&pool, conversation, after).await?;
//!     if !rows.is_empty() { return Ok(rows); }
//!     tokio::select! { () = w.changed() => {}, () = tokio::time::sleep(Duration::from_secs(1)) => {} }
//! }
//! ```

use std::collections::HashMap;
use std::fmt::Write as _;
use std::future::poll_fn;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_postgres::{AsyncMessage, Notification};

use crate::{Error, Pool};

/// Reconnect backoff: 0.5 s doubling to 5 s (platform 4.3), reset once a connection is listening.
const BACKOFF_FIRST: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// A quiet connection is checked this often (bounded by the same), so a half-open socket is noticed before TCP
/// keepalives would.
const PING: Duration = Duration::from_secs(10);
const PING_TIMEOUT: Duration = Duration::from_secs(5);

/// A waiter's slot: a channel, and the payload key or `None` for "any notification on the channel".
type Slot = (String, Option<String>);

#[derive(Debug, Default)]
struct Shared {
    channels: Vec<String>,
    waiters: Mutex<HashMap<Slot, watch::Sender<u64>>>,
    reconnects: AtomicU64,
    notifications: AtomicU64,
}

impl Shared {
    fn bump(&self, slot: &Slot) {
        let waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(tx) = waiters.get(slot) {
            tx.send_modify(|n| *n = n.wrapping_add(1));
        }
    }

    fn wake_all(&self) {
        let mut waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
        waiters.retain(|_, tx| tx.receiver_count() > 0);
        for tx in waiters.values() {
            tx.send_modify(|n| *n = n.wrapping_add(1));
        }
    }

    fn deliver(&self, n: &Notification) {
        self.notifications.fetch_add(1, Ordering::Relaxed);
        let payload = n.payload();
        let key = payload.split_once(':').map_or(payload, |(k, _)| k);
        self.bump(&(n.channel().to_owned(), Some(key.to_owned())));
        self.bump(&(n.channel().to_owned(), None));
    }
}

/// The process's LISTEN connection. Dropping it (or [`Listener::stop`]) closes the connection; waiters made from it
/// then wake only by their own timeouts.
#[derive(Debug)]
pub struct Listener {
    shared: Arc<Shared>,
    connected: watch::Receiver<bool>,
    stop: Mutex<Option<oneshot::Sender<()>>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

/// Wakes when a notification for its slot arrives or the listener reconnects.
#[derive(Debug)]
pub struct Waiter {
    rx: watch::Receiver<u64>,
    shared: Arc<Shared>,
    slot: Slot,
}

impl Listener {
    /// Starts listening on `channels` (lowercase identifiers: `[a-z_][a-z0-9_]*`, at most 63 bytes).
    ///
    /// # Errors
    /// [`Error::Config`] for an invalid or repeated channel name, or none.
    pub fn start(pool: &Pool, channels: &[&str]) -> Result<Self, Error> {
        if channels.is_empty() {
            return Err(Error::Config("listener without channels".into()));
        }
        let mut names: Vec<String> = Vec::new();
        for c in channels {
            let valid = !c.is_empty()
                && c.len() <= 63
                && c.bytes()
                    .enumerate()
                    .all(|(i, b)| b == b'_' || b.is_ascii_lowercase() || (i > 0 && b.is_ascii_digit()));
            if !valid || names.iter().any(|n| n == c) {
                return Err(Error::Config(format!("bad listen channel {c:?}")));
            }
            names.push((*c).to_owned());
        }
        let shared = Arc::new(Shared {
            channels: names,
            ..Shared::default()
        });
        let (connected_tx, connected) = watch::channel(false);
        let (stop_tx, stop_rx) = oneshot::channel();
        let task = tokio::spawn(run(pool.clone(), shared.clone(), connected_tx, stop_rx));
        Ok(Self {
            shared,
            connected,
            stop: Mutex::new(Some(stop_tx)),
            task: Mutex::new(Some(task)),
        })
    }

    /// A waiter woken by notifications on `channel` whose payload's key (the part before the first `:`, or the whole
    /// payload) is `key`, and by every reconnect.
    ///
    /// # Panics
    /// When `channel` is not one this listener listens on (a programming error: it would never wake).
    #[must_use]
    pub fn waiter(&self, channel: &str, key: &str) -> Waiter {
        self.make((channel.to_owned(), Some(key.to_owned())))
    }

    /// A waiter woken by every notification on `channel`, and by every reconnect.
    ///
    /// # Panics
    /// When `channel` is not one this listener listens on.
    #[must_use]
    pub fn channel_waiter(&self, channel: &str) -> Waiter {
        self.make((channel.to_owned(), None))
    }

    fn make(&self, slot: Slot) -> Waiter {
        assert!(
            self.shared.channels.contains(&slot.0),
            "not listening on channel {:?}",
            slot.0
        );
        let rx = {
            let mut waiters = self.shared.waiters.lock().unwrap_or_else(PoisonError::into_inner);
            waiters
                .entry(slot.clone())
                .or_insert_with(|| watch::channel(0).0)
                .subscribe()
        };
        Waiter {
            rx,
            shared: self.shared.clone(),
            slot,
        }
    }

    /// Whether the LISTEN connection is up now. Readiness waits for its first `true` only (platform 3.2).
    #[must_use]
    pub fn connected(&self) -> watch::Receiver<bool> {
        self.connected.clone()
    }

    /// Resolves once the first connection is listening.
    pub async fn ready(&self) {
        let mut rx = self.connected.clone();
        let _ = rx.wait_for(|up| *up).await;
    }

    /// How many times the connection was re-established (a metric).
    #[must_use]
    pub fn reconnects(&self) -> u64 {
        self.shared.reconnects.load(Ordering::Relaxed)
    }

    /// How many notifications arrived (a metric).
    #[must_use]
    pub fn notifications(&self) -> u64 {
        self.shared.notifications.load(Ordering::Relaxed)
    }

    /// Closes the connection and stops reconnecting (drain). Takes `&self`, so a listener shared behind an `Arc`
    /// can be stopped; a second call returns at once.
    pub async fn stop(&self) {
        if let Some(stop) = self.stop.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = stop.send(());
        }
        let task = self.task.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        if let Some(task) = self.task.get_mut().unwrap_or_else(PoisonError::into_inner).take() {
            task.abort();
        }
    }
}

impl Waiter {
    /// Resolves at the next notification for this slot or reconnect after the waiter was made or last woke.
    pub async fn changed(&mut self) {
        // the sender lives in the map while this receiver exists, so this only returns Err if the map was cleared
        let _ = self.rx.changed().await;
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        let mut waiters = self.shared.waiters.lock().unwrap_or_else(PoisonError::into_inner);
        // this waiter's receiver is still alive here; it is the last one when the count is 1
        if waiters.get(&self.slot).is_some_and(|tx| tx.receiver_count() <= 1) {
            waiters.remove(&self.slot);
        }
    }
}

async fn run(pool: Pool, shared: Arc<Shared>, connected: watch::Sender<bool>, mut stop: oneshot::Receiver<()>) {
    let mut backoff = BACKOFF_FIRST;
    let mut ever = false;
    loop {
        let session = tokio::select! {
            _ = &mut stop => return,
            s = listen(&pool, &shared) => s,
        };
        match session {
            Ok(mut s) => {
                connected.send_replace(true);
                backoff = BACKOFF_FIRST;
                if ever {
                    shared.reconnects.fetch_add(1, Ordering::Relaxed);
                    tracing::info!("listener: reconnected, waking every waiter");
                    // anything notified while the connection was down is found by the waiters' re-queries
                    shared.wake_all();
                }
                ever = true;
                let stopped = s.serve(&shared, &mut stop).await;
                connected.send_replace(false);
                if stopped {
                    return;
                }
                tracing::warn!("listener: connection lost");
            }
            Err(e) => tracing::warn!(error = %e, "listener: cannot listen"),
        }
        tokio::select! {
            _ = &mut stop => return,
            () = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

struct Session {
    client: tokio_postgres::Client,
    notifications: mpsc::UnboundedReceiver<Notification>,
    driver: JoinHandle<()>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

/// Opens a connection, drives it on a task that forwards notifications, and runs `LISTEN` for every channel.
async fn listen(pool: &Pool, shared: &Shared) -> Result<Session, Error> {
    let (client, mut connection) = pool.dedicated_raw().await?;
    let (tx, notifications) = mpsc::unbounded_channel();
    let driver = tokio::spawn(async move {
        loop {
            match poll_fn(|cx| connection.poll_message(cx)).await {
                Some(Ok(AsyncMessage::Notification(n))) => {
                    if tx.send(n).is_err() {
                        return;
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    tracing::debug!(error = %e, "listener connection ended");
                    return;
                }
                None => return,
            }
        }
    });
    let session = Session {
        client,
        notifications,
        driver,
    };
    let mut sql = String::new();
    for c in &shared.channels {
        let _ = write!(sql, "listen \"{c}\";");
    }
    session.client.batch_execute(&sql).await?;
    Ok(session)
}

impl Session {
    /// Delivers notifications until the connection is lost (`false`) or a stop is asked (`true`).
    async fn serve(&mut self, shared: &Shared, stop: &mut oneshot::Receiver<()>) -> bool {
        loop {
            tokio::select! {
                _ = &mut *stop => return true,
                n = self.notifications.recv() => match n {
                    Some(n) => shared.deliver(&n),
                    None => return false, // the driver ended: the connection is gone
                },
                () = tokio::time::sleep(PING) => {
                    if !matches!(tokio::time::timeout(PING_TIMEOUT, self.client.simple_query("select 1")).await, Ok(Ok(_))) {
                        return false;
                    }
                }
            }
        }
    }
}
