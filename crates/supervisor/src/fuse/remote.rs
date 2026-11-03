//! The magpie side of the FUSE layer (supervisor 9.5): every cc call with `Aviary-Lease`, at most 16 at once, 30 s
//! per attempt, retried with the same `opId` (0.2 s doubling to 10 s) for as long as magpie is unavailable for
//! everyone, EIO after 10 min when only this op keeps failing while magpie answers other calls (`cc.write_stuck`),
//! and the lease state (`lease_lost`: EIO to everything in flight, unleased until the lease loop takes a new one).

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use magpie_api::client::{ClientError, SandboxCalls};
use tokio::sync::{Notify, Semaphore, watch};
use uuid::Uuid;

use super::errno::{EINTR, EIO, Failure, classify, refused_unexecuted};

/// The timings of 9.5 and 20 (the constants; tests shorten them).
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub attempt: Duration,
    pub retry_first: Duration,
    pub retry_max: Duration,
    pub stuck_op: Duration,
    /// How recent another successful call must be for an op to count as stuck (twice the heartbeat).
    pub healthy_within: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        use crate::limits as l;
        Self {
            attempt: l::MAGPIE_ATTEMPT_TIMEOUT,
            retry_first: l::CC_WRITE_RETRY_FIRST,
            retry_max: l::CC_WRITE_RETRY_MAX,
            stuck_op: l::STUCK_OP,
            healthy_within: l::HEARTBEAT_EVERY * 2,
        }
    }
}

/// Why a call ended without a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// A definitive answer (no magpie operation was made), or EIO while unleased.
    Errno(i32),
    /// `offset_mismatch` / `offset_beyond_eof`.
    Repair,
    /// magpie said `lease_lost` (the caller answers EIO; the lease is already marked lost).
    LeaseLost,
    /// INTERRUPT arrived while no attempt had reached magpie: EINTR, and no magpie operation was made.
    Interrupted,
    /// The same op kept failing for `STUCK_OP` while magpie answered other calls.
    Stuck(String),
}

impl CallError {
    /// The errno the FUSE reply carries.
    #[must_use]
    pub fn errno(&self) -> i32 {
        match self {
            CallError::Errno(e) => *e,
            CallError::Interrupted => EINTR,
            CallError::Repair | CallError::LeaseLost | CallError::Stuck(_) => EIO,
        }
    }
}

/// Per-request control: INTERRUPT, and whether an attempt may have reached magpie.
#[derive(Debug, Default)]
pub struct OpCtl {
    /// A journal replay after a restart: namespace checks are left to magpie (the derived `opId` returns the stored
    /// result), never answered from a tree that may already show the request's own effect.
    pub replay: bool,
    interrupted: AtomicBool,
    executed: AtomicBool,
    /// An earlier attempt may have been executed (a connection error, a timeout, a gateway answer).
    sticky: AtomicBool,
    wake: Notify,
}

fn prior_executed(ctl: &OpCtl) -> bool {
    ctl.sticky.load(Ordering::SeqCst)
}

impl OpCtl {
    /// The control of a journal replay (or not).
    #[must_use]
    pub fn new(replay: bool) -> Self {
        Self {
            replay,
            ..Self::default()
        }
    }

    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::SeqCst);
        self.wake.notify_waiters();
    }
    /// Whether any attempt may have been executed by magpie.
    #[must_use]
    pub fn executed(&self) -> bool {
        self.executed.load(Ordering::SeqCst)
    }
}

/// Why one attempt of a streamed read ended without a result.
#[derive(Debug)]
pub enum AttemptError {
    /// magpie's answer, or the connection's.
    Client(ClientError),
    /// No byte (or no next chunk) within the attempt timeout: retried like a timed-out call.
    TimedOut,
    /// A local failure (the cache file): this errno at once, no retry.
    Local(i32),
}

impl From<ClientError> for AttemptError {
    fn from(e: ClientError) -> Self {
        AttemptError::Client(e)
    }
}

/// What one attempt gets: an owned client and the lease (so the attempt's future borrows nothing).
#[derive(Clone, Debug)]
pub struct Calls {
    pub client: magpie_api::client::Client,
    pub lease: Option<Uuid>,
}

impl Calls {
    /// The sandbox calls under the lease.
    #[must_use]
    pub fn sb(&self) -> SandboxCalls<'_> {
        self.client.sandbox(self.lease)
    }
}

/// The current lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lease {
    pub id: Uuid,
    pub epoch: u64,
}

#[derive(Debug, Default)]
struct Blocked {
    next: u64,
    since: HashMap<u64, Instant>,
}

/// magpie for the FUSE layer.
pub struct Remote {
    client: magpie_api::client::Client,
    timing: Timing,
    /// Permits of the calls bounded by one attempt's timeout.
    sem: Semaphore,
    /// Permits of the streamed whole-file fetches (no bound on the whole body), apart so they never hold the
    /// writes' permits.
    streams: Semaphore,
    lease: watch::Sender<Option<Lease>>,
    base: Instant,
    last_ok: AtomicU64,
    blocked: Mutex<Blocked>,
    reachable: AtomicBool,
}

impl std::fmt::Debug for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Remote")
            .field("lease", &*self.lease.borrow())
            .finish_non_exhaustive()
    }
}

struct BlockedGuard<'a>(&'a Remote, u64);

impl Drop for BlockedGuard<'_> {
    fn drop(&mut self) {
        self.0
            .blocked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .since
            .remove(&self.1);
    }
}

impl Remote {
    #[must_use]
    pub fn new(client: magpie_api::client::Client, timing: Timing) -> Self {
        Self {
            client,
            timing,
            sem: Semaphore::new(crate::limits::MAGPIE_CONCURRENCY - crate::limits::MAGPIE_STREAMS),
            streams: Semaphore::new(crate::limits::MAGPIE_STREAMS),
            lease: watch::channel(None).0,
            base: Instant::now(),
            last_ok: AtomicU64::new(0),
            blocked: Mutex::new(Blocked::default()),
            reachable: AtomicBool::new(true),
        }
    }

    /// The client (lease-free calls: the tree, files).
    #[must_use]
    pub fn client(&self) -> &magpie_api::client::Client {
        &self.client
    }

    #[must_use]
    pub fn lease(&self) -> Option<Lease> {
        *self.lease.borrow()
    }

    pub fn set_lease(&self, l: Option<Lease>) {
        self.lease.send_replace(l);
    }

    /// Marks the lease `l` lost (no effect if a newer one was taken meanwhile); whether it was the current one.
    pub fn lost(&self, l: Lease) -> bool {
        self.lease.send_if_modified(|cur| {
            if *cur == Some(l) {
                *cur = None;
                true
            } else {
                false
            }
        })
    }

    /// Records a successful exchange with magpie (any call; WP-23's heartbeat calls it too): the stuck rule needs
    /// evidence that magpie answers others.
    pub fn record_ok(&self) {
        #[allow(clippy::cast_possible_truncation)] // u64 nanoseconds cover 584 years
        let n = self.base.elapsed().as_nanos() as u64 + 1;
        self.last_ok.store(n, Ordering::SeqCst);
        self.reachable.store(true, Ordering::SeqCst);
    }

    fn ok_since(&self, t: Instant) -> bool {
        let last = self.last_ok.load(Ordering::SeqCst);
        if last == 0 {
            return false;
        }
        let at = self.base + Duration::from_nanos(last - 1);
        at > t && at.elapsed() <= self.timing.healthy_within
    }

    /// Requests waiting in the retry loop, and the oldest's age.
    #[must_use]
    pub fn blocked(&self) -> (u32, Option<Duration>) {
        let b = self.blocked.lock().unwrap_or_else(PoisonError::into_inner);
        #[allow(clippy::cast_possible_truncation)] // at most FUSE_SLOTS
        (b.since.len() as u32, b.since.values().min().map(Instant::elapsed))
    }

    #[must_use]
    pub fn reachable(&self) -> bool {
        self.reachable.load(Ordering::SeqCst)
    }

    fn block(&self) -> BlockedGuard<'_> {
        let mut b = self.blocked.lock().unwrap_or_else(PoisonError::into_inner);
        b.next += 1;
        let id = b.next;
        b.since.insert(id, Instant::now());
        BlockedGuard(self, id)
    }

    /// A state-changing call under the lease, with the retry rule. `f` makes one attempt (called again for each
    /// retry, with the same `opId` inside). `sent` runs after every attempt that came back (fault injection).
    ///
    /// # Errors
    /// [`CallError`]: a definitive answer, the repair, a lost lease, an interrupt, a stuck op.
    pub async fn call<T, F, Fut>(&self, ctl: &OpCtl, what: &str, f: F, sent: &(dyn Fn() + Sync)) -> Result<T, CallError>
    where
        F: Fn(Calls) -> Fut,
        Fut: Future<Output = Result<T, ClientError>>,
    {
        let f = |c: Calls| {
            let fut = f(c);
            async move { fut.await.map_err(AttemptError::Client) }
        };
        self.retry(ctl, what, true, Some(self.timing.attempt), &self.sem, f, sent, None)
            .await
    }

    /// A lease-free read (tree, file bytes) with the same retry rule.
    ///
    /// # Errors
    /// As [`Remote::call`].
    pub async fn read<T, F, Fut>(&self, ctl: &OpCtl, what: &str, f: F, sent: &(dyn Fn() + Sync)) -> Result<T, CallError>
    where
        F: Fn(Calls) -> Fut,
        Fut: Future<Output = Result<T, ClientError>>,
    {
        let f = |c: Calls| {
            let fut = f(c);
            async move { fut.await.map_err(AttemptError::Client) }
        };
        self.retry(ctl, what, false, Some(self.timing.attempt), &self.sem, f, sent, None)
            .await
    }

    /// A lease-free streamed read (a whole file): no bound on the whole attempt, since a large body may take longer
    /// than one attempt's timeout to arrive. `f` bounds the wait for the answer and for each next chunk with
    /// [`Remote::attempt_timeout`] itself ([`AttemptError::TimedOut`]), and resumes from what earlier attempts
    /// fetched. It holds a permit of its own pool ([`crate::limits::MAGPIE_STREAMS`]), never a write's.
    /// `progress` counts what arrived: an attempt that advanced it restarts the stuck clock and the backoff (a fetch
    /// that keeps advancing between dropped connections is not stuck). The same retry rule otherwise.
    ///
    /// # Errors
    /// As [`Remote::call`].
    pub async fn read_streamed<T, F, Fut>(
        &self,
        ctl: &OpCtl,
        what: &str,
        f: F,
        progress: &(dyn Fn() -> u64 + Sync),
    ) -> Result<T, CallError>
    where
        F: Fn(Calls) -> Fut,
        Fut: Future<Output = Result<T, AttemptError>>,
    {
        self.retry(ctl, what, false, None, &self.streams, f, &|| {}, Some(progress))
            .await
    }

    /// The timeout of one attempt (30 s): a streamed read's bound on each wait.
    #[must_use]
    pub fn attempt_timeout(&self) -> Duration {
        self.timing.attempt
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)] // the one retry loop of 9.5
    async fn retry<T, F, Fut>(
        &self,
        ctl: &OpCtl,
        what: &str,
        leased: bool,
        limit: Option<Duration>,
        sem: &Semaphore,
        f: F,
        sent: &(dyn Fn() + Sync),
        progress: Option<&(dyn Fn() -> u64 + Sync)>,
    ) -> Result<T, CallError>
    where
        F: Fn(Calls) -> Fut,
        Fut: Future<Output = Result<T, AttemptError>>,
    {
        let mut rx = self.lease.subscribe();
        let lease = *rx.borrow_and_update();
        if leased && lease.is_none() {
            return Err(CallError::Errno(EIO));
        }
        let mut wait = self.timing.retry_first;
        let mut first_fail: Option<Instant> = None;
        let mut guard = None;
        loop {
            if ctl.interrupted.load(Ordering::SeqCst) && !ctl.executed() {
                return Err(CallError::Interrupted);
            }
            let before = progress.map(|p| p());
            let attempt = async {
                let permit = sem.acquire().await;
                // from here the attempt may reach magpie: an INTERRUPT no longer answers EINTR
                ctl.executed.store(true, Ordering::SeqCst);
                let calls = Calls {
                    client: self.client.clone(),
                    lease: if leased { lease.map(|l| l.id) } else { None },
                };
                let r = match limit {
                    Some(l) => tokio::time::timeout(l, f(calls))
                        .await
                        .unwrap_or(Err(AttemptError::TimedOut)),
                    None => f(calls).await,
                };
                drop(permit);
                r
            };
            let mut attempt = std::pin::pin!(attempt);
            let res = loop {
                let woken = ctl.wake.notified();
                if ctl.interrupted.load(Ordering::SeqCst) && !ctl.executed() {
                    return Err(CallError::Interrupted);
                }
                tokio::select! {
                    r = &mut attempt => break r,
                    () = woken => {}
                    _ = rx.changed(), if leased => return Err(CallError::Errno(EIO)),
                }
            };
            sent();
            if let (Some(p), Some(b)) = (progress, before)
                && p() > b
            {
                // the attempt advanced: whatever failed it, this op is not stuck since its first failure
                first_fail = None;
                wait = self.timing.retry_first;
            }
            let err = match res {
                Ok(v) => {
                    self.record_ok();
                    return Ok(v);
                }
                Err(AttemptError::Client(e)) => e,
                Err(AttemptError::Local(n)) => return Err(CallError::Errno(n)),
                Err(AttemptError::TimedOut) => {
                    ctl.sticky.store(true, Ordering::SeqCst);
                    tracing::info!(what, "magpie attempt timed out");
                    let t = *first_fail.get_or_insert_with(Instant::now);
                    if self.stuck(t) {
                        return Err(CallError::Stuck(format!("{what}: timed out")));
                    }
                    guard.get_or_insert_with(|| self.block());
                    self.backoff(&mut wait, ctl, &mut rx, leased).await?;
                    continue;
                }
            };
            match classify(&err) {
                Failure::Retry => {
                    if refused_unexecuted(&err) && !prior_executed(ctl) {
                        // magpie refused before doing anything: an INTERRUPT may still answer EINTR
                        ctl.executed.store(false, Ordering::SeqCst);
                    } else {
                        ctl.sticky.store(true, Ordering::SeqCst);
                    }
                    self.reachable.store(false, Ordering::SeqCst);
                    let t = *first_fail.get_or_insert_with(Instant::now);
                    if self.stuck(t) {
                        tracing::error!(what, error = %err, "cc op stuck");
                        return Err(CallError::Stuck(format!("{what}: {err}")));
                    }
                    guard.get_or_insert_with(|| self.block());
                    self.backoff(&mut wait, ctl, &mut rx, leased).await?;
                }
                Failure::Errno(n) => {
                    self.record_ok();
                    return Err(CallError::Errno(n));
                }
                Failure::Repair => {
                    self.record_ok();
                    return Err(CallError::Repair);
                }
                Failure::LeaseLost => {
                    self.record_ok();
                    if let Some(l) = lease
                        && self.lost(l)
                    {
                        tracing::warn!(what, epoch = l.epoch, "lease lost");
                    }
                    return Err(CallError::LeaseLost);
                }
            }
        }
    }

    fn stuck(&self, first_fail: Instant) -> bool {
        first_fail.elapsed() >= self.timing.stuck_op && self.ok_since(first_fail)
    }

    async fn backoff(
        &self,
        wait: &mut Duration,
        ctl: &OpCtl,
        rx: &mut watch::Receiver<Option<Lease>>,
        leased: bool,
    ) -> Result<(), CallError> {
        // the Notified exists before the interrupt check, so an INTERRUPT between the two is not missed
        let woken = ctl.wake.notified();
        if ctl.interrupted.load(Ordering::SeqCst) && !ctl.executed() {
            return Err(CallError::Interrupted);
        }
        tokio::select! {
            () = tokio::time::sleep(*wait) => {}
            () = woken, if !ctl.executed() => return Err(CallError::Interrupted),
            _ = rx.changed(), if leased => return Err(CallError::Errno(EIO)),
        }
        *wait = (*wait * 2).min(self.timing.retry_max);
        Ok(())
    }
}

/// A shared handle.
pub type SharedRemote = Arc<Remote>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::sync::atomic::AtomicU32;

    use super::*;

    fn remote(stuck_op: Duration) -> Arc<Remote> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = magpie_api::client::Client::new(reqwest::Client::new(), "http://127.0.0.1:9", "unused");
        Arc::new(Remote::new(
            client,
            Timing {
                attempt: Duration::from_secs(5),
                retry_first: Duration::from_millis(10),
                retry_max: Duration::from_millis(20),
                stuck_op,
                healthy_within: Duration::from_secs(30),
            },
        ))
    }

    /// Six attempts of 150 ms, each failing (a dropped connection) while magpie answers others, against a 300 ms
    /// stuck rule: stuck when nothing arrives, done when every attempt advanced.
    async fn streamed(advance: bool) -> Result<(), CallError> {
        let r = remote(Duration::from_millis(300));
        let (got, attempts) = (AtomicU64::new(0), AtomicU32::new(0));
        let (rr, g, a) = (&r, &got, &attempts);
        r.read_streamed(
            &OpCtl::default(),
            "cc/file",
            |_| async move {
                rr.record_ok();
                tokio::time::sleep(Duration::from_millis(150)).await;
                if advance {
                    g.fetch_add(1, Ordering::SeqCst);
                }
                if a.fetch_add(1, Ordering::SeqCst) + 1 < 6 {
                    Err(AttemptError::TimedOut)
                } else {
                    Ok(())
                }
            },
            &|| got.load(Ordering::SeqCst),
        )
        .await
    }

    #[tokio::test]
    async fn a_streamed_fetch_that_keeps_advancing_is_not_stuck() {
        assert!(matches!(streamed(false).await, Err(CallError::Stuck(_))));
        assert_eq!(streamed(true).await, Ok(()));
    }

    /// Streamed fetches that hold their permits for ever never take a call's.
    #[tokio::test]
    async fn streamed_fetches_never_hold_the_calls_permits() {
        let r = remote(Duration::from_secs(600));
        r.set_lease(Some(Lease {
            id: Uuid::now_v7(),
            epoch: 1,
        }));
        let forever = Arc::new(Notify::new());
        for _ in 0..crate::limits::MAGPIE_CONCURRENCY {
            let (r, n) = (r.clone(), forever.clone());
            tokio::spawn(async move {
                let n2 = n.clone();
                r.read_streamed(
                    &OpCtl::default(),
                    "cc/file",
                    move |_| {
                        let n = n2.clone();
                        async move {
                            n.notified().await;
                            Ok(())
                        }
                    },
                    &|| 0,
                )
                .await
            });
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let w = tokio::time::timeout(
            Duration::from_secs(2),
            r.call(&OpCtl::default(), "cc/write", |_| async { Ok(7) }, &|| {}),
        )
        .await;
        assert_eq!(w.unwrap(), Ok(7), "a write finds a permit");
    }
}
