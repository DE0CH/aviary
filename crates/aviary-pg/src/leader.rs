//! Leadership by session advisory locks on one dedicated connection (platform 4.3, hermit 10.3, magpie 15).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tokio_postgres::Client;

use crate::{LockKey, Pool};

/// The retry interval of platform 4.3: a follower tries the lock every 5 s; the holder checks its connection as
/// often.
pub const RETRY: Duration = Duration::from_secs(5);

/// How long [`Leader::confirm`]'s `select 1` may take.
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(2);

/// One process's bid for one or more singleton roles, all on ONE dedicated connection (hermit 10.3 runs the janitor
/// and the scaler on "its dedicated leader connection"; platform 11.4 budgets one leader connection per pod).
///
/// A task holds the connection and tries `pg_try_advisory_lock(key)` for each key it does not hold every `retry`;
/// while it holds any, it checks the connection every `retry` (`select 1`, bounded by `retry`). Every key is lost the
/// moment the connection ends or a check fails; [`Leader::release`] (drain) unlocks them explicitly so successors take
/// over within one retry. After a loss the task waits one `retry` before bidding again.
///
/// Callers run [`Leader::confirm`] (or [`Leader::confirm_key`]) before every pass of their loop: it is `true` only
/// when the key is held and a `select 1` on the leader connection answers now, so a half-open connection stops the
/// loop at once rather than after the next background check. [`Leader::is_leader`] / [`Leader::watch`] give the
/// background view, to wait for leadership.
#[derive(Debug)]
pub struct Leader {
    keys: Vec<LockKey>,
    states: Vec<watch::Receiver<bool>>,
    client: Arc<Mutex<Option<Arc<Client>>>>,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl Leader {
    /// Starts bidding for `key` with the retry interval of platform 4.3 ([`RETRY`]).
    #[must_use]
    pub fn start(pool: &Pool, key: LockKey) -> Self {
        Self::start_with(pool, key, RETRY)
    }

    /// Starts bidding for `key`, trying and checking every `retry` (magpie's job leader retries every 10 s).
    #[must_use]
    pub fn start_with(pool: &Pool, key: LockKey, retry: Duration) -> Self {
        Self::start_many(pool, &[key], retry)
    }

    /// Starts bidding for every key of `keys` on one dedicated connection, trying and checking every `retry`. Each
    /// key leads on its own: another process may hold one key while this one holds another.
    ///
    /// # Panics
    /// When `keys` is empty or holds a key twice.
    #[must_use]
    pub fn start_many(pool: &Pool, keys: &[LockKey], retry: Duration) -> Self {
        assert!(!keys.is_empty(), "Leader::start_many needs a key");
        for (i, k) in keys.iter().enumerate() {
            assert!(!keys[..i].contains(k), "Leader::start_many: {k:?} given twice");
        }
        let (senders, states): (Vec<_>, Vec<_>) = keys.iter().map(|_| watch::channel(false)).unzip();
        let client = Arc::new(Mutex::new(None));
        let (stop_tx, stop_rx) = oneshot::channel();
        let task = tokio::spawn(bid(
            Bid {
                pool: pool.clone(),
                keys: keys.to_vec(),
                retry,
                states: senders,
                client: client.clone(),
            },
            stop_rx,
        ));
        Self {
            keys: keys.to_vec(),
            states,
            client,
            stop: Some(stop_tx),
            task,
        }
    }

    /// Whether this process leads the (first) key now, and its changes.
    #[must_use]
    pub fn is_leader(&self) -> watch::Receiver<bool> {
        self.states[0].clone()
    }

    /// Whether this process leads `key` now, and its changes; `None` for a key it does not bid for.
    #[must_use]
    pub fn watch(&self, key: &LockKey) -> Option<watch::Receiver<bool>> {
        self.index(key).map(|i| self.states[i].clone())
    }

    /// [`Leader::confirm_key`] for the (first) key.
    pub async fn confirm(&self) -> bool {
        self.confirm_index(0).await
    }

    /// `true` when this process holds `key` and its leader connection answers `select 1` within
    /// [`CONFIRM_TIMEOUT`]: run it before every pass of the loop `key` guards (hermit 10.3). `false` for a key it
    /// does not bid for.
    pub async fn confirm_key(&self, key: &LockKey) -> bool {
        match self.index(key) {
            Some(i) => self.confirm_index(i).await,
            None => false,
        }
    }

    fn index(&self, key: &LockKey) -> Option<usize> {
        self.keys.iter().position(|k| k == key)
    }

    async fn confirm_index(&self, i: usize) -> bool {
        if !*self.states[i].borrow() {
            return false;
        }
        let client = self
            .client
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let Some(client) = client else {
            return false;
        };
        let answered = matches!(
            tokio::time::timeout(CONFIRM_TIMEOUT, client.simple_query("select 1")).await,
            Ok(Ok(_))
        );
        // still held after the round trip (the task did not see the connection end meanwhile)
        answered && *self.states[i].borrow()
    }

    /// Gives every lock up (drain, platform 3.3): unlocks those held, closes the connection and stops bidding.
    pub async fn release(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = (&mut self.task).await;
    }
}

impl Drop for Leader {
    fn drop(&mut self) {
        // without release(): the connection closes with the task, which frees the locks server-side
        self.task.abort();
    }
}

struct Bid {
    pool: Pool,
    keys: Vec<LockKey>,
    retry: Duration,
    states: Vec<watch::Sender<bool>>,
    client: Arc<Mutex<Option<Arc<Client>>>>,
}

impl Bid {
    fn set_client(&self, c: Option<Arc<Client>>) {
        *self.client.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = c;
    }

    fn lose_all(&self) {
        for s in &self.states {
            s.send_replace(false);
        }
    }
}

enum Outcome {
    Stop,
    Lost,
}

type Stop = oneshot::Receiver<()>;

async fn bid(b: Bid, mut stop: Stop) {
    loop {
        let outcome = session(&b, &mut stop).await;
        b.lose_all();
        b.set_client(None);
        if matches!(outcome, Outcome::Stop) {
            return;
        }
        tokio::select! {
            _ = &mut stop => return,
            () = tokio::time::sleep(b.retry) => {}
        }
    }
}

/// One dedicated connection's life: every `retry`, try each key not held and check the connection; until the
/// connection is lost or a stop is asked (then unlock what is held and close).
async fn session(b: &Bid, stop: &mut Stop) -> Outcome {
    let connected = tokio::select! {
        _ = &mut *stop => return Outcome::Stop,
        c = b.pool.dedicated() => c,
    };
    let (client, mut connection) = match connected {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "leader: cannot connect");
            return Outcome::Lost;
        }
    };
    let client = Arc::new(client);
    b.set_client(Some(client.clone()));
    let mut held = vec![false; b.keys.len()];
    loop {
        // bid for what is not held; the attempt doubles as the connection check
        for (i, key) in b.keys.iter().enumerate() {
            if held[i] {
                continue;
            }
            let attempt = tokio::select! {
                _ = &mut *stop => return close(b, client, connection, &held).await,
                a = tokio::time::timeout(b.retry, key.try_lock(&client)) => a,
            };
            match attempt {
                Ok(Ok(true)) => {
                    held[i] = true;
                    b.states[i].send_replace(true);
                    tracing::info!(key = ?key, "leader: lock held");
                }
                Ok(Ok(false)) => {}
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "leader: lock attempt failed");
                    return lost(&held);
                }
                Err(_) => {
                    tracing::warn!("leader: lock attempt timed out");
                    return lost(&held);
                }
            }
        }
        tokio::select! {
            _ = &mut *stop => return close(b, client, connection, &held).await,
            _ = &mut connection => return lost(&held),
            () = tokio::time::sleep(b.retry) => {}
        }
        if held.iter().any(|h| *h) {
            match tokio::time::timeout(b.retry, client.simple_query("select 1")).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "leader: connection check failed");
                    return lost(&held);
                }
                Err(_) => {
                    tracing::warn!("leader: connection check timed out");
                    return lost(&held);
                }
            }
        }
    }
}

fn lost(held: &[bool]) -> Outcome {
    if held.iter().any(|h| *h) {
        tracing::warn!("leader: locks lost");
    }
    Outcome::Lost
}

/// Unlocks what is held and closes the session; the server frees the locks with the session in any case.
async fn close(b: &Bid, client: Arc<Client>, connection: JoinHandle<()>, held: &[bool]) -> Outcome {
    // callers stop before a successor can take over
    b.lose_all();
    for (key, _) in b.keys.iter().zip(held).filter(|(_, h)| **h) {
        tracing::info!(key = ?key, "leader: releasing");
        if let Ok(Err(e)) = tokio::time::timeout(RETRY, key.unlock(&client)).await {
            tracing::warn!(error = %e, "leader: unlock failed (the closing session frees it)");
        }
    }
    b.set_client(None);
    drop(client);
    let _ = tokio::time::timeout(RETRY, connection).await;
    Outcome::Stop
}
