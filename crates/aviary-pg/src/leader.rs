//! Leadership by a session advisory lock on a dedicated connection (platform 4.3, hermit 10.3, magpie 15).

use std::time::Duration;

use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tokio_postgres::Client;

use crate::{LockKey, Pool};

/// The retry interval of platform 4.3: a follower tries the lock every 5 s; the holder checks its connection as
/// often.
pub const RETRY: Duration = Duration::from_secs(5);

/// One process's bid for a singleton role. A task holds a dedicated connection, tries `pg_try_advisory_lock(key)`
/// every `retry`, and while it holds the lock checks the connection every `retry` (`select 1`, bounded by `retry`).
/// The lock is lost the moment the connection ends or a check fails; [`Leader::release`] (drain) unlocks it
/// explicitly so a successor takes over within one retry. After a loss the task waits one `retry` before bidding
/// again.
///
/// Callers check [`Leader::is_leader`] before every pass of their loop and stop at once when it turns `false`.
#[derive(Debug)]
pub struct Leader {
    state: watch::Receiver<bool>,
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
        let (tx, rx) = watch::channel(false);
        let (stop_tx, stop_rx) = oneshot::channel();
        let task = tokio::spawn(bid(pool.clone(), key, retry, tx, stop_rx));
        Self {
            state: rx,
            stop: Some(stop_tx),
            task,
        }
    }

    /// Whether this process leads now, and its changes.
    #[must_use]
    pub fn is_leader(&self) -> watch::Receiver<bool> {
        self.state.clone()
    }

    /// Gives the lock up (drain, platform 3.3): unlocks it if held, closes the connection and stops bidding.
    pub async fn release(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = (&mut self.task).await;
    }
}

impl Drop for Leader {
    fn drop(&mut self) {
        // without release(): the connection closes with the task, which frees the lock server-side
        self.task.abort();
    }
}

enum Outcome {
    Stop,
    Lost,
}

type Stop = oneshot::Receiver<()>;

async fn bid(pool: Pool, key: LockKey, retry: Duration, state: watch::Sender<bool>, mut stop: Stop) {
    loop {
        let outcome = session(&pool, &key, retry, &state, &mut stop).await;
        state.send_replace(false);
        if matches!(outcome, Outcome::Stop) {
            return;
        }
        tokio::select! {
            _ = &mut stop => return,
            () = tokio::time::sleep(retry) => {}
        }
    }
}

/// One dedicated connection's life: bid until the lock is held, hold it until the connection is lost or a stop is
/// asked (then unlock and close).
async fn session(pool: &Pool, key: &LockKey, retry: Duration, state: &watch::Sender<bool>, stop: &mut Stop) -> Outcome {
    let connected = tokio::select! {
        _ = &mut *stop => return Outcome::Stop,
        c = pool.dedicated() => c,
    };
    let (client, mut connection) = match connected {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "leader: cannot connect");
            return Outcome::Lost;
        }
    };
    loop {
        let attempt = tokio::select! {
            _ = &mut *stop => return close(client, connection, key, false).await,
            a = tokio::time::timeout(retry, key.try_lock(&client)) => a,
        };
        match attempt {
            Ok(Ok(true)) => break,
            Ok(Ok(false)) => {}
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "leader: lock attempt failed");
                return Outcome::Lost;
            }
            Err(_) => {
                tracing::warn!("leader: lock attempt timed out");
                return Outcome::Lost;
            }
        }
        tokio::select! {
            _ = &mut *stop => return close(client, connection, key, false).await,
            _ = &mut connection => return Outcome::Lost,
            () = tokio::time::sleep(retry) => {}
        }
    }
    state.send_replace(true);
    tracing::info!("leader: lock held");
    loop {
        tokio::select! {
            _ = &mut *stop => {
                state.send_replace(false);
                tracing::info!("leader: releasing");
                return close(client, connection, key, true).await;
            }
            _ = &mut connection => break,
            () = tokio::time::sleep(retry) => {}
        }
        match tokio::time::timeout(retry, client.simple_query("select 1")).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "leader: connection check failed");
                break;
            }
            Err(_) => {
                tracing::warn!("leader: connection check timed out");
                break;
            }
        }
    }
    state.send_replace(false);
    tracing::warn!("leader: lock lost");
    Outcome::Lost
}

/// Unlocks (when held) and closes the session; the server frees the lock with the session in any case.
async fn close(client: Client, connection: JoinHandle<()>, key: &LockKey, held: bool) -> Outcome {
    if held && let Ok(Err(e)) = tokio::time::timeout(RETRY, key.unlock(&client)).await {
        tracing::warn!(error = %e, "leader: unlock failed (the closing session frees it)");
    }
    drop(client);
    let _ = tokio::time::timeout(RETRY, connection).await;
    Outcome::Stop
}
